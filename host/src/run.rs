//! Parsing, type-checking and running a pipeline.

use crate::builtins;
use crate::pump::{self, Spec, pipe::Counters};
use crate::stats::MemoryTracker;
use crate::types::{self, describe};
use anyhow::{Context as _, Result, bail};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};
use wasmtime::component::types::ComponentItem;
use wasmtime::component::{Component, Func, Linker, ResourceTable, StreamAny, Type, Val};
use wasmtime::{AsContextMut, Config, Engine, Store};
use wasmtime_wasi::p2::pipe::MemoryOutputPipe;
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

/// Who copies rows between two components.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Copy {
    /// The host pump (compiled copy plans, no per-row host allocations).
    Host,
    /// Wasmtime's built-in guest-to-guest copy (lifts each row into `Val`s).
    /// Pipes that need the host (`take`, projections) are still pumped.
    Wasmtime,
}

pub struct Options {
    /// Directory holding `<tool>.wasm` files.
    pub tools: PathBuf,
    pub copy: Copy,
    /// Run each component in its own Store on its own thread. Requires
    /// `Copy::Host`, since only the pump can carry rows between Stores.
    pub parallel: bool,
    /// Capture the components' stdout/stderr instead of inheriting them.
    pub capture: bool,
    /// Where to keep compiled components between runs (`None` = no cache).
    pub cache: Option<PathBuf>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            tools: PathBuf::from("target/wasm32-wasip2/release"),
            copy: Copy::Host,
            parallel: true,
            capture: false,
            cache: Some(PathBuf::from("target/wp-cache")),
        }
    }
}

/// What a pipe between two stages carried.
#[derive(Clone, Debug)]
pub struct PipeReport {
    pub label: String,
    pub how: &'static str,
    pub elems: u64,
    pub rows: u64,
    pub batches: u64,
    pub bytes: u64,
    pub reallocs: u64,
    pub lift_ns: u64,
    pub lower_ns: u64,
}

pub struct Report {
    /// The last stage's return value.
    pub result: Val,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub load: Duration,
    pub run: Duration,
    /// Pipes the host carried.
    pub pipes: Vec<PipeReport>,
    /// Total linear memory the components grew to.
    pub guest_memory: u64,
    /// Bytes read by `cat` stages.
    pub cat_bytes: u64,
    /// Whether stages ran in parallel (one Store and thread each).
    pub parallel: bool,
}

impl Report {
    /// The last stage's result as a row count, if it returned an integer.
    pub fn count(&self) -> Option<u64> {
        match self.result {
            Val::U64(n) => Some(n),
            Val::U32(n) => Some(n as u64),
            _ => None,
        }
    }
}

struct Ctx {
    wasi: WasiCtx,
    table: ResourceTable,
    mem: MemoryTracker,
}

impl WasiView for Ctx {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView { ctx: &mut self.wasi, table: &mut self.table }
    }
}

enum Step {
    Tool { name: String, args: Vec<String> },
    Take(u64),
    Cat(PathBuf),
}

impl Step {
    fn label(&self) -> String {
        match self {
            Step::Tool { name, .. } => name.clone(),
            Step::Take(n) => format!("take {n}"),
            Step::Cat(p) => format!("cat {}", p.display()),
        }
    }
}

fn parse(pipeline: &str) -> Result<Vec<Step>> {
    let steps: Vec<Step> = pipeline
        .split('|')
        .map(|part| {
            let mut words = part.split_whitespace().map(String::from);
            let name = words.next().context("empty stage in pipeline")?;
            let args: Vec<String> = words.collect();
            Ok(match name.as_str() {
                "take" => match args.as_slice() {
                    [n] => Step::Take(n.parse().context("take expects a number")?),
                    _ => bail!("usage: take N"),
                },
                "cat" => match args.as_slice() {
                    [p] => Step::Cat(PathBuf::from(p)),
                    _ => bail!("usage: cat FILE"),
                },
                _ => Step::Tool { name, args },
            })
        })
        .collect::<Result<_>>()?;
    if steps.len() < 2 {
        bail!("a pipeline needs at least a producer and a consumer");
    }
    if matches!(steps.first(), Some(Step::Take(_))) {
        bail!("a pipeline can't start with take");
    }
    if !matches!(steps.last(), Some(Step::Tool { .. })) {
        bail!("a pipeline must end with a component");
    }
    for (i, s) in steps.iter().enumerate() {
        if i > 0 && matches!(s, Step::Cat(_)) {
            bail!("cat can only start a pipeline");
        }
        if i > 0 && matches!(s, Step::Take(_)) && matches!(steps[i - 1], Step::Cat(_)) {
            bail!("take can't follow cat yet");
        }
    }
    Ok(steps)
}

/// Convert a command-line word into a value of the parameter's type.
fn arg(ty: &Type, word: Option<&str>) -> Result<Val> {
    let Some(w) = word else {
        return match ty {
            Type::Option(_) => Ok(Val::Option(None)),
            _ => bail!("missing argument of type {}", describe(ty)),
        };
    };
    Ok(match ty {
        // `_` skips an optional argument so a later one can be given.
        Type::Option(_) if w == "_" => Val::Option(None),
        Type::Option(o) => Val::Option(Some(Box::new(arg(&o.ty(), Some(w))?))),
        Type::Bool => Val::Bool(w.parse()?),
        Type::U8 => Val::U8(w.parse()?),
        Type::U16 => Val::U16(w.parse()?),
        Type::U32 => Val::U32(w.parse()?),
        Type::U64 => Val::U64(w.parse()?),
        Type::S8 => Val::S8(w.parse()?),
        Type::S16 => Val::S16(w.parse()?),
        Type::S32 => Val::S32(w.parse()?),
        Type::S64 => Val::S64(w.parse()?),
        Type::Float32 => Val::Float32(w.parse()?),
        Type::Float64 => Val::Float64(w.parse()?),
        Type::String => Val::String(w.into()),
        other => bail!("can't pass {} on the command line", describe(other)),
    })
}

fn stream_str(elem: &Option<Type>) -> String {
    match elem {
        Some(t) => format!("stream<{}>", describe(t)),
        None => "stream".into(),
    }
}

/// Where a component's input stream comes from.
enum Input {
    None,
    Cat(PathBuf),
    /// A pipe from an earlier component.
    Pipe(usize),
}

/// A pipe between two components, possibly through `take`s.
struct PipePlan {
    label: String,
    spec: Spec,
    in_layout: (u32, u32),
    out_layout: (u32, u32),
    /// The host must carry it (else Wasmtime may copy directly in one Store).
    pumped: bool,
    counters: Arc<Counters>,
}

struct Stage {
    name: String,
    component: Component,
    args: Vec<Val>,
    input: Input,
    /// Index of the pipe this stage writes into, if any.
    output: Option<usize>,
}

pub struct Runner {
    engine: Engine,
    linker: Linker<Ctx>,
    components: HashMap<String, Component>,
    opts: Options,
}

impl Runner {
    pub fn new(opts: Options) -> Result<Runner> {
        let mut config = Config::new();
        config.wasm_component_model_async(true);
        let engine = Engine::new(&config)?;
        let mut linker = Linker::<Ctx>::new(&engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
        Ok(Runner { engine, linker, components: HashMap::new(), opts })
    }

    pub fn options(&self) -> &Options {
        &self.opts
    }

    pub fn options_mut(&mut self) -> &mut Options {
        &mut self.opts
    }

    fn component(&mut self, name: &str) -> Result<Component> {
        if let Some(c) = self.components.get(name) {
            return Ok(c.clone());
        }
        // Cargo names `parse-log`'s library `parse_log.wasm`.
        let mut path = self.opts.tools.join(format!("{name}.wasm"));
        if !path.exists() {
            path = self.opts.tools.join(format!("{}.wasm", name.replace('-', "_")));
        }
        let bytes = std::fs::read(&path)
            .with_context(|| format!("no tool named `{name}` (looked for {})", path.display()))?;
        let component = self.compile(&bytes)?;
        self.components.insert(name.to_string(), component.clone());
        Ok(component)
    }

    /// Compile a component, reusing a cached native build when possible.
    fn compile(&self, bytes: &[u8]) -> Result<Component> {
        let Some(dir) = &self.opts.cache else {
            return Ok(Component::new(&self.engine, bytes)?);
        };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut h);
        self.engine.precompile_compatibility_hash().hash(&mut h);
        let file = dir.join(format!("{:016x}.cwasm", h.finish()));
        if file.exists() {
            // SAFETY: the cache only holds files this program wrote with
            // `precompile_component` for a compatible engine.
            if let Ok(c) = unsafe { Component::deserialize_file(&self.engine, &file) } {
                return Ok(c);
            }
        }
        let compiled = self.engine.precompile_component(bytes)?;
        std::fs::create_dir_all(dir).ok();
        let tmp = file.with_extension(format!("{}.tmp", std::process::id()));
        if std::fs::write(&tmp, &compiled).is_ok() {
            std::fs::rename(&tmp, &file).ok();
        }
        // SAFETY: produced by `precompile_component` on this engine just now.
        Ok(unsafe { Component::deserialize(&self.engine, &compiled)? })
    }

    /// The `run` export's parameter and result types.
    fn signature(&self, name: &str, component: &Component) -> Result<(Vec<Type>, Option<Type>)> {
        let ty = component.component_type();
        let run = ty.get_export(&self.engine, "run").with_context(|| format!("`{name}` has no `run` export"))?;
        let ComponentItem::ComponentFunc(f) = run.ty else { bail!("`{name}`'s `run` isn't a function") };
        Ok((f.params().map(|(_, t)| t).collect(), f.results().next()))
    }

    /// Resolve and type-check every step, before running anything.
    fn plan(&mut self, steps: &[Step]) -> Result<(Vec<Stage>, Vec<PipePlan>)> {
        let copy = self.opts.copy;
        let parallel = self.opts.parallel && copy == Copy::Host;
        let mut stages: Vec<Stage> = Vec::new();
        let mut pipes: Vec<PipePlan> = Vec::new();
        // The stream flowing into the next step: (element type, source).
        let mut flowing: Option<(Option<Type>, Input)> = None;
        let mut limit: Option<u64> = None;
        let mut via = Vec::new();
        for (i, step) in steps.iter().enumerate() {
            match step {
                Step::Cat(path) => flowing = Some((Some(Type::U8), Input::Cat(path.clone()))),
                Step::Take(n) => {
                    limit = Some(limit.map_or(*n, |l| l.min(*n)));
                    via.push(format!("take {n}"));
                }
                Step::Tool { name, args } => {
                    let component = self.component(name)?;
                    let (mut params, result) = self.signature(name, &component)?;
                    let mut input = Input::None;
                    if let Some((have, source)) = flowing.take() {
                        let want = match params.first() {
                            Some(Type::Stream(s)) => s.ty(),
                            _ => bail!(
                                "`{name}` doesn't read a stream, but `{}` produces {}",
                                steps[i - 1].label(),
                                stream_str(&have)
                            ),
                        };
                        let ok = match (&want, &have) {
                            (Some(w), Some(h)) => types::accepts(w, h),
                            (None, None) => true,
                            _ => false,
                        };
                        if !ok {
                            bail!(
                                "`{name}` expects {}, but `{}` produces {}",
                                stream_str(&want),
                                steps[i - 1].label(),
                                stream_str(&have),
                            );
                        }
                        params.remove(0);
                        input = match source {
                            Input::Pipe(producer) => {
                                let (w, h) = (want.unwrap(), have.unwrap());
                                let projected = describe(&w) != describe(&h);
                                let pumped = parallel
                                    || limit.is_some()
                                    || projected
                                    || (copy == Copy::Host && !types::is_flat(&h));
                                let mut label = stages[producer].name.clone();
                                for v in via.drain(..) {
                                    label.push_str(&format!(" → {v}"));
                                }
                                label.push_str(&format!(" → {name}"));
                                // Copy only what the consumer reads. Columnar batches
                                // need every column `take` might cut, else fall back.
                                let mut need = types::need(&w, &h);
                                let mut shape = types::batch_shape(&h);
                                if let Some(sh) = &shape {
                                    match sh.only(&need) {
                                        Some(only) => shape = Some(only),
                                        None => need = pump::Need::All,
                                    }
                                }
                                let pipe = PipePlan {
                                    label,
                                    spec: Spec { limit: limit.take(), shape, need },
                                    in_layout: types::layout(&h)?,
                                    out_layout: types::layout(&w)?,
                                    pumped,
                                    counters: Arc::new(Counters::default()),
                                };
                                if projected && !pumped {
                                    unreachable!();
                                }
                                pipes.push(pipe);
                                stages[producer].output = Some(pipes.len() - 1);
                                Input::Pipe(pipes.len() - 1)
                            }
                            other => other,
                        };
                    }
                    if args.len() > params.len() {
                        bail!("`{name}` takes {} argument(s), got {}", params.len(), args.len());
                    }
                    let args = params
                        .iter()
                        .enumerate()
                        .map(|(k, t)| arg(t, args.get(k).map(|s| s.as_str())))
                        .collect::<Result<Vec<_>>>()
                        .with_context(|| format!("arguments to `{name}`"))?;
                    let out = match result {
                        Some(Type::Stream(s)) => Some(s.ty()),
                        _ => None,
                    };
                    if i + 1 < steps.len() && out.is_none() {
                        bail!("`{name}` doesn't produce a stream, so nothing can follow it");
                    }
                    if i + 1 == steps.len() && out.is_some() {
                        bail!("`{name}` produces a stream, but nothing reads it");
                    }
                    stages.push(Stage { name: name.clone(), component, args, input, output: None });
                    if let Some(elem) = out {
                        flowing = Some((elem, Input::Pipe(stages.len() - 1)));
                    }
                }
            }
        }
        Ok((stages, pipes))
    }

    fn store(&self, stdout: &MemoryOutputPipe, stderr: &MemoryOutputPipe) -> Store<Ctx> {
        let mut wasi = WasiCtx::builder();
        if self.opts.capture {
            wasi.stdout(stdout.clone()).stderr(stderr.clone());
        } else {
            wasi.inherit_stdio();
        }
        let ctx = Ctx { wasi: wasi.build(), table: ResourceTable::new(), mem: MemoryTracker::default() };
        let mut store = Store::new(&self.engine, ctx);
        store.limiter(|c| &mut c.mem);
        store
    }

    async fn instantiate(&self, store: &mut Store<Ctx>, stage: &Stage) -> Result<Func> {
        // Interfaces that only declare types (like a shared row record) are
        // still imported as instances; give them empty ones.
        let mut linker = self.linker.clone();
        for (import, item) in stage.component.component_type().imports(&self.engine) {
            if let ComponentItem::ComponentInstance(inst) = item.ty {
                let types_only = inst.exports(&self.engine).all(|(_, e)| matches!(e.ty, ComponentItem::Type(_)));
                if types_only {
                    linker.instance(import)?;
                }
            }
        }
        let instance = linker.instantiate_async(&mut *store, &stage.component).await?;
        instance.get_func(&mut *store, "run").context("no `run` export")
    }

    pub async fn run(&mut self, pipeline: &str) -> Result<Report> {
        let started = Instant::now();
        let steps = parse(pipeline)?;
        let (stages, pipes) = self.plan(&steps)?;
        let parallel = self.opts.parallel && self.opts.copy == Copy::Host;
        let stdout = MemoryOutputPipe::new(usize::MAX);
        let stderr = MemoryOutputPipe::new(usize::MAX);
        let cat_bytes = Arc::new(AtomicU64::new(0));

        let (result, guest_memory, load, run) = if parallel {
            self.run_parallel(&stages, &pipes, &stdout, &stderr, &cat_bytes, started)?
        } else {
            self.run_single(&stages, &pipes, &stdout, &stderr, &cat_bytes, started).await?
        };

        let pipes = pipes
            .iter()
            .filter(|p| p.pumped)
            .map(|p| {
                let c = &p.counters;
                PipeReport {
                    label: p.label.clone(),
                    how: "host pump",
                    elems: c.elems.load(Relaxed),
                    rows: c.rows.load(Relaxed),
                    batches: c.batches.load(Relaxed),
                    bytes: c.bytes.load(Relaxed),
                    reallocs: c.reallocs.load(Relaxed),
                    lift_ns: c.lift_ns.load(Relaxed),
                    lower_ns: c.lower_ns.load(Relaxed),
                }
            })
            .collect();
        Ok(Report {
            result,
            stdout: stdout.contents().to_vec(),
            stderr: stderr.contents().to_vec(),
            load,
            run,
            pipes,
            guest_memory,
            cat_bytes: cat_bytes.load(Relaxed),
            parallel,
        })
    }

    /// Every component in one Store, driven by one task.
    async fn run_single(
        &self,
        stages: &[Stage],
        pipes: &[PipePlan],
        stdout: &MemoryOutputPipe,
        stderr: &MemoryOutputPipe,
        cat_bytes: &Arc<AtomicU64>,
        started: Instant,
    ) -> Result<(Val, u64, Duration, Duration)> {
        let mut store = self.store(stdout, stderr);
        let mut funcs = Vec::new();
        for stage in stages {
            funcs.push(self.instantiate(&mut store, stage).await?);
        }
        let load = started.elapsed();
        let run_started = Instant::now();
        let result = store
            .run_concurrent(async |acc| -> Result<Val> {
                // Streams produced so far, by pipe index.
                let mut produced: Vec<Option<StreamAny>> = pipes.iter().map(|_| None).collect();
                let mut last = None;
                for (stage, func) in stages.iter().zip(&funcs) {
                    let mut params = Vec::with_capacity(stage.args.len() + 1);
                    match &stage.input {
                        Input::None => {}
                        Input::Cat(path) => {
                            let s = acc.with(|mut a| builtins::cat(a.as_context_mut(), path, cat_bytes.clone()))?;
                            params.push(Val::Stream(s));
                        }
                        Input::Pipe(p) => {
                            let pipe = &pipes[*p];
                            let mut s = produced[*p].take().context("pipe has no producer")?;
                            if pipe.pumped {
                                s = acc.with(|mut a| {
                                    pump::pump(
                                        a.as_context_mut(),
                                        s,
                                        pipe.spec.clone(),
                                        pipe.in_layout,
                                        pipe.out_layout,
                                        pipe.counters.clone(),
                                    )
                                })?;
                            }
                            params.push(Val::Stream(s));
                        }
                    }
                    params.extend(stage.args.iter().cloned());
                    let mut results = vec![Val::Bool(false)];
                    func.call_concurrent(acc, &params, &mut results).await?;
                    match (results.pop().unwrap(), stage.output) {
                        (Val::Stream(s), Some(p)) => produced[p] = Some(s),
                        (v, _) => last = Some(v),
                    }
                }
                last.context("pipeline ended without a result")
            })
            .await??;
        let run = run_started.elapsed();
        Ok((result, store.data().mem.linear_bytes, load, run))
    }

    /// Each component in its own Store on its own thread; the host pumps
    /// rows between them, so stages run truly in parallel.
    fn run_parallel(
        &self,
        stages: &[Stage],
        pipes: &[PipePlan],
        stdout: &MemoryOutputPipe,
        stderr: &MemoryOutputPipe,
        cat_bytes: &Arc<AtomicU64>,
        started: Instant,
    ) -> Result<(Val, u64, Duration, Duration)> {
        // Split each pipe into its halves up front.
        let mut halves: Vec<(Option<pump::pipe::PumpIn>, Option<pump::pipe::PumpOut>, Option<pump::Finished>)> =
            pipes
                .iter()
                .map(|p| {
                    let (i, o, f) = pump::pipe::new(p.spec.clone(), p.counters.clone());
                    (Some(i), Some(o), Some(f))
                })
                .collect();
        let ready = std::sync::Barrier::new(stages.len() + 1);
        let run_started = std::sync::Mutex::new(None::<Instant>);
        let load = std::sync::Mutex::new(Duration::ZERO);

        let outcomes: Vec<Result<(Option<Val>, u64)>> = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for stage in stages {
                let input = match &stage.input {
                    Input::Pipe(p) => Some((halves[*p].1.take().unwrap(), pipes[*p].out_layout)),
                    _ => None,
                };
                let output = stage.output.map(|p| {
                    (halves[p].0.take().unwrap(), halves[p].2.take().unwrap(), pipes[p].in_layout)
                });
                let (ready, run_started, load) = (&ready, &run_started, &load);
                handles.push(scope.spawn(move || -> Result<(Option<Val>, u64)> {
                    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
                    rt.block_on(async move {
                        let mut store = self.store(stdout, stderr);
                        let func = self.instantiate(&mut store, stage).await;
                        // Start every stage together once all are instantiated.
                        ready.wait();
                        let func = func?;
                        {
                            let mut l = load.lock().unwrap();
                            *l = (*l).max(started.elapsed());
                            run_started.lock().unwrap().get_or_insert_with(Instant::now);
                        }
                        let value = store
                            .run_concurrent(async move |acc| -> Result<Option<Val>> {
                                let mut params = Vec::with_capacity(stage.args.len() + 1);
                                match (&stage.input, input) {
                                    (Input::Cat(path), _) => {
                                        let s = acc.with(|mut a| builtins::cat(a.as_context_mut(), path, cat_bytes.clone()))?;
                                        params.push(Val::Stream(s));
                                    }
                                    (Input::Pipe(_), Some((pout, layout))) => {
                                        let s = acc.with(|mut a| pump::open_output(a.as_context_mut(), layout, pout))?;
                                        params.push(Val::Stream(s));
                                    }
                                    _ => {}
                                }
                                params.extend(stage.args.iter().cloned());
                                let mut results = vec![Val::Bool(false)];
                                func.call_concurrent(acc, &params, &mut results).await?;
                                match (results.pop().unwrap(), output) {
                                    (Val::Stream(s), Some((pin, finished, layout))) => {
                                        acc.with(|mut a| pump::attach_input(a.as_context_mut(), s, layout, pin))?;
                                        // Keep driving this Store until the producer is done.
                                        finished.await;
                                        // If it was cut off early (take, or the consumer quit),
                                        // let it see that and wind down before its Store goes.
                                        for _ in 0..8 {
                                            tokio::task::yield_now().await;
                                        }
                                        Ok(None)
                                    }
                                    (v, _) => Ok(Some(v)),
                                }
                            })
                            .await??;
                        Ok((value, store.data().mem.linear_bytes))
                    })
                }));
            }
            ready.wait();
            handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err(anyhow::anyhow!("a stage panicked")))).collect()
        });
        let run = run_started.lock().unwrap().map(|t| t.elapsed()).unwrap_or_default();

        let mut result = None;
        let mut memory = 0;
        for (stage, outcome) in stages.iter().zip(outcomes) {
            let (value, mem) = outcome.with_context(|| format!("stage `{}`", stage.name))?;
            memory += mem;
            if value.is_some() {
                result = value;
            }
        }
        let load = *load.lock().unwrap();
        Ok((result.context("pipeline ended without a result")?, memory, load, run))
    }
}

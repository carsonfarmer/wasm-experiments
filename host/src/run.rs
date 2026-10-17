//! Parsing, type-checking and running a pipeline.

use crate::builtins;
use crate::pump::{self, pipe::Counters, pipe::Limit};
use crate::stats::MemoryTracker;
use crate::types::{self, describe};
use anyhow::{Context as _, Result, bail};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};
use wasmtime::component::types::ComponentItem;
use wasmtime::component::{Component, Func, Linker, ResourceTable, Type, Val};
use wasmtime::{AsContextMut, Config, Engine, Store};
use wasmtime_wasi::p2::pipe::MemoryOutputPipe;
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

/// Who copies rows between two components.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Copy {
    /// The host pump (compiled copy plans, no per-row host allocations).
    Host,
    /// Wasmtime's built-in guest-to-guest copy (lifts each row into `Val`s).
    Wasmtime,
}

pub struct Options {
    /// Directory holding `<tool>.wasm` files.
    pub tools: PathBuf,
    pub copy: Copy,
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
    /// Pipes the host carried (pumped pipes and built-ins).
    pub pipes: Vec<PipeReport>,
    /// Total linear memory the components grew to.
    pub guest_memory: u64,
    /// Bytes read by `cat` stages.
    pub cat_bytes: u64,
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

/// A step with everything resolved and checked.
enum Planned {
    Tool { name: String, func: Func, args: Vec<Val>, takes_stream: bool },
    Take { n: u64, elem: Type },
    Cat(PathBuf),
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

    pub fn set_copy(&mut self, copy: Copy) {
        self.opts.copy = copy;
    }

    fn component(&mut self, name: &str) -> Result<Component> {
        if let Some(c) = self.components.get(name) {
            return Ok(c.clone());
        }
        let path = self.opts.tools.join(format!("{name}.wasm"));
        let bytes = std::fs::read(&path).with_context(|| {
            format!("no tool named `{name}` (looked for {})", path.display())
        })?;
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
        let tmp = file.with_extension("tmp");
        if std::fs::write(&tmp, &compiled).is_ok() {
            std::fs::rename(&tmp, &file).ok();
        }
        // SAFETY: produced by `precompile_component` on this engine just now.
        Ok(unsafe { Component::deserialize(&self.engine, &compiled)? })
    }

    pub async fn run(&mut self, pipeline: &str) -> Result<Report> {
        let steps = parse(pipeline)?;
        let started = Instant::now();

        let stdout = MemoryOutputPipe::new(usize::MAX);
        let stderr = MemoryOutputPipe::new(usize::MAX);
        let mut wasi = WasiCtx::builder();
        if self.opts.capture {
            wasi.stdout(stdout.clone()).stderr(stderr.clone());
        } else {
            wasi.inherit_stdio();
        }
        let ctx = Ctx { wasi: wasi.build(), table: ResourceTable::new(), mem: MemoryTracker::default() };
        let mut store = Store::new(&self.engine, ctx);
        store.limiter(|c| &mut c.mem);

        // Load and instantiate every component; resolve each step.
        let mut funcs = Vec::new();
        for step in &steps {
            let Step::Tool { name, .. } = step else {
                funcs.push(None);
                continue;
            };
            let component = self.component(name)?;
            // Interfaces that only declare types (like a shared row record)
            // are still imported as instances; give them empty ones.
            let mut linker = self.linker.clone();
            for (import, item) in component.component_type().imports(&self.engine) {
                if let ComponentItem::ComponentInstance(inst) = item.ty {
                    let types_only = inst
                        .exports(&self.engine)
                        .all(|(_, e)| matches!(e.ty, ComponentItem::Type(_)));
                    if types_only {
                        linker.instance(import)?;
                    }
                }
            }
            let instance = linker.instantiate_async(&mut store, &component).await?;
            let func = instance
                .get_func(&mut store, "run")
                .with_context(|| format!("`{name}` has no `run` export"))?;
            funcs.push(Some(func));
        }
        let load = started.elapsed();

        // Type-check every pipe before running anything.
        let mut planned = Vec::new();
        let mut flowing: Option<Option<Type>> = None; // element type of the incoming stream
        let mut elems: Vec<Option<Type>> = Vec::new(); // element type flowing out of each step
        for (i, step) in steps.iter().enumerate() {
            match step {
                Step::Cat(path) => {
                    if i != 0 {
                        bail!("cat can only start a pipeline");
                    }
                    planned.push(Planned::Cat(path.clone()));
                    flowing = Some(Some(Type::U8));
                }
                Step::Take(n) => {
                    let elem = flowing.clone().flatten().context("take needs a typed stream")?;
                    if matches!(steps[i - 1], Step::Cat(_)) {
                        bail!("take can't follow cat yet");
                    }
                    planned.push(Planned::Take { n: *n, elem: elem.clone() });
                    flowing = Some(Some(elem));
                }
                Step::Tool { name, args } => {
                    let func = funcs[i].unwrap();
                    let ty = func.ty(&store);
                    let mut params: Vec<Type> = ty.params().map(|(_, t)| t).collect();
                    let takes_stream = flowing.is_some();
                    if let Some(expected) = &flowing {
                        let got = match params.first() {
                            Some(Type::Stream(s)) => Some(s.ty()),
                            _ => None,
                        };
                        if got.as_ref().map(stream_str) != Some(stream_str(expected)) {
                            bail!(
                                "`{name}` expects {}, but `{}` produces {}",
                                params.first().map(describe).unwrap_or_else(|| "no input".into()),
                                steps[i - 1].label(),
                                stream_str(expected),
                            );
                        }
                        params.remove(0);
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
                    let out = match ty.results().next() {
                        Some(Type::Stream(s)) => Some(s.ty()),
                        _ => None,
                    };
                    if i + 1 < steps.len() && out.is_none() {
                        bail!("`{name}` doesn't produce a stream, so nothing can follow it");
                    }
                    if i + 1 == steps.len() && out.is_some() {
                        bail!("`{name}` produces a stream, but nothing reads it");
                    }
                    planned.push(Planned::Tool { name: name.clone(), func, args, takes_stream });
                    flowing = out;
                }
            }
            elems.push(flowing.clone().flatten());
        }

        // Decide which pipes the host carries.
        let copy = self.opts.copy;
        let mut pipes: Vec<(String, &'static str, Arc<Counters>)> = Vec::new();
        let cat_bytes = Arc::new(AtomicU64::new(0));

        let run_started = Instant::now();
        let result = store
            .run_concurrent(async |acc| -> Result<Val> {
                let mut stream: Option<wasmtime::component::StreamAny> = None;
                for (i, step) in planned.iter().enumerate() {
                    match step {
                        Planned::Cat(path) => {
                            let s = acc.with(|mut a| builtins::cat(a.as_context_mut(), path, cat_bytes.clone()))?;
                            stream = Some(s);
                        }
                        Planned::Take { n, elem } => {
                            let input = stream.take().unwrap();
                            let limit = match types::batch_shape(elem) {
                                Some(shape) => Limit::Rows(*n, shape),
                                None => Limit::Elems(*n),
                            };
                            let layout = types::layout(elem)?;
                            let counters = Arc::new(Counters::default());
                            pipes.push((
                                format!("{} → take {n}", steps[i - 1].label()),
                                "host pump",
                                counters.clone(),
                            ));
                            let s = acc.with(|mut a| pump::pump(a.as_context_mut(), input, limit, layout, counters))?;
                            stream = Some(s);
                        }
                        Planned::Tool { name, func, args, takes_stream } => {
                            let mut params = Vec::with_capacity(args.len() + 1);
                            if *takes_stream {
                                let mut input = stream.take().unwrap();
                                let prev = &planned[i - 1];
                                let elem = elems[i - 1].clone();
                                // Pump pipes between two components whose rows
                                // hold pointers; flat rows already move by memcpy.
                                let pumped = copy == Copy::Host
                                    && matches!(prev, Planned::Tool { .. })
                                    && elem.as_ref().is_some_and(|t| !types::is_flat(t));
                                if pumped {
                                    let elem = elem.unwrap();
                                    let counters = Arc::new(Counters::default());
                                    pipes.push((
                                        format!("{} → {name}", steps[i - 1].label()),
                                        "host pump",
                                        counters.clone(),
                                    ));
                                    let layout = types::layout(&elem)?;
                                    input = acc.with(|mut a| {
                                        pump::pump(a.as_context_mut(), input, Limit::None, layout, counters)
                                    })?;
                                }
                                params.push(Val::Stream(input));
                            }
                            params.extend(args.iter().cloned());
                            let mut results = vec![Val::Bool(false)];
                            func.call_concurrent(acc, &params, &mut results).await?;
                            let r = results.pop().unwrap();
                            match r {
                                Val::Stream(s) => stream = Some(s),
                                other => return Ok(other),
                            }
                        }
                    }
                }
                bail!("pipeline ended without a result")
            })
            .await??;
        let run = run_started.elapsed();

        let pipes = pipes
            .into_iter()
            .map(|(label, how, c)| PipeReport {
                label,
                how,
                elems: c.elems.load(Relaxed),
                rows: c.rows.load(Relaxed),
                batches: c.batches.load(Relaxed),
                bytes: c.bytes.load(Relaxed),
                reallocs: c.reallocs.load(Relaxed),
                lift_ns: c.lift_ns.load(Relaxed),
                lower_ns: c.lower_ns.load(Relaxed),
            })
            .collect();
        Ok(Report {
            result,
            stdout: stdout.contents().to_vec(),
            stderr: stderr.contents().to_vec(),
            load,
            run,
            pipes,
            guest_memory: store.data().mem.linear_bytes,
            cat_bytes: cat_bytes.load(Relaxed),
        })
    }
}

/// Convenience: a runner with default options rooted at `tools`.
pub fn runner_for(tools: &Path, copy: Copy, capture: bool) -> Result<Runner> {
    Runner::new(Options { tools: tools.to_path_buf(), copy, capture, ..Options::default() })
}

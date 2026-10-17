//! Parsing, type-checking and running a pipeline.

use crate::builtins;
use crate::pump::{self, Counters, Layout, Spec};
use crate::stats::MemoryTracker;
use anyhow::{Context as _, Result, anyhow, bail, ensure};
use serde::Serialize;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wasmtime::component::types::ComponentItem;
use wasmtime::component::{Accessor, Component, Func, Linker, ResourceTable, StreamAny, Type, Val};
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
    /// Directory holding the `<tool>.wasm` files.
    pub tools: PathBuf,
    pub copy: Copy,
    /// Run each component in its own Store on its own thread (needs `Copy::Host`).
    pub parallel: bool,
    /// Capture the components' stdout/stderr instead of inheriting them.
    pub capture: bool,
    /// Where compiled components are cached between runs.
    pub cache: Option<PathBuf>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            tools: "target/wasm32-wasip2/release".into(),
            copy: Copy::Host,
            parallel: true,
            capture: false,
            cache: Some("target/wp-cache".into()),
        }
    }
}

/// What the host pump carried through one pipe.
#[derive(Clone, Debug, Serialize)]
pub struct PipeReport {
    pub label: String,
    pub elems: u64,
    pub rows: u64,
    pub batches: u64,
    pub bytes: u64,
    pub reallocs: u64,
    pub lift_ms: f64,
    pub lower_ms: f64,
}

pub struct Report {
    /// The last stage's return value.
    pub result: Val,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub load: Duration,
    pub run: Duration,
    pub pipes: Vec<PipeReport>,
    /// Total linear memory the components grew to.
    pub guest_memory: u64,
    /// Bytes read by `cat`.
    pub cat_bytes: u64,
    pub parallel: bool,
}

impl Report {
    /// The last stage's result as a row count, if it's an integer.
    pub fn count(&self) -> Option<u64> {
        match self.result {
            Val::U64(n) => Some(n),
            Val::U32(n) => Some(n.into()),
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

/// One `|`-separated step as written.
enum Step {
    Tool(String, Vec<String>),
    Take(u64),
    Cat(PathBuf),
}

impl Step {
    fn label(&self) -> String {
        match self {
            Step::Tool(name, _) => name.clone(),
            Step::Take(n) => format!("take {n}"),
            Step::Cat(p) => format!("cat {}", p.display()),
        }
    }
}

fn parse(pipeline: &str) -> Result<Vec<Step>> {
    let steps = pipeline
        .split('|')
        .map(|part| {
            let words: Vec<&str> = part.split_whitespace().collect();
            Ok(match words.as_slice() {
                [] => bail!("empty stage in pipeline"),
                ["take", n] => Step::Take(n.parse().context("take expects a number")?),
                ["take", ..] => bail!("usage: take N"),
                ["cat", path] => Step::Cat(path.into()),
                ["cat", ..] => bail!("usage: cat FILE"),
                [name, args @ ..] => Step::Tool(name.to_string(), args.iter().map(|a| a.to_string()).collect()),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(steps.len() >= 2, "a pipeline needs at least a producer and a consumer");
    ensure!(!matches!(steps[0], Step::Take(_)), "a pipeline can't start with take");
    ensure!(matches!(steps.last(), Some(Step::Tool(..))), "a pipeline must end with a component");
    for w in steps.windows(2) {
        ensure!(!matches!(w[1], Step::Cat(_)), "cat can only start a pipeline");
        ensure!(!matches!((&w[0], &w[1]), (Step::Cat(_), Step::Take(_))), "take can't follow cat yet");
    }
    Ok(steps)
}

/// Convert a command-line word into a value of the parameter's type.
fn arg(ty: &Type, word: Option<&str>) -> Result<Val> {
    Ok(match (ty, word) {
        (Type::Option(_), None | Some("_")) => Val::Option(None), // `_` skips an optional argument
        (_, None) => bail!("missing argument of type {}", Layout::of(ty)?),
        (Type::Option(o), w) => Val::Option(Some(Box::new(arg(&o.ty(), w)?))),
        (Type::String, Some(w)) => Val::String(w.into()),
        (Type::Bool, Some(w)) => Val::Bool(w.parse()?),
        (Type::U8, Some(w)) => Val::U8(w.parse()?),
        (Type::U16, Some(w)) => Val::U16(w.parse()?),
        (Type::U32, Some(w)) => Val::U32(w.parse()?),
        (Type::U64, Some(w)) => Val::U64(w.parse()?),
        (Type::S8, Some(w)) => Val::S8(w.parse()?),
        (Type::S16, Some(w)) => Val::S16(w.parse()?),
        (Type::S32, Some(w)) => Val::S32(w.parse()?),
        (Type::S64, Some(w)) => Val::S64(w.parse()?),
        (Type::Float32, Some(w)) => Val::Float32(w.parse()?),
        (Type::Float64, Some(w)) => Val::Float64(w.parse()?),
        (other, _) => bail!("can't pass {} on the command line", Layout::of(other)?),
    })
}

/// A component step, resolved and type-checked.
struct Stage {
    name: String,
    component: Component,
    args: Vec<Val>,
    input: Input,
    /// The pipe this stage writes into.
    output: Option<usize>,
}

enum Input {
    None,
    Cat(PathBuf),
    Pipe(usize),
}

/// A stream between two components (through any `take`s).
struct Pipe {
    label: String,
    have: Arc<Layout>,
    want: Arc<Layout>,
    limit: Option<u64>,
    /// Carried by the host pump (otherwise Wasmtime copies it directly).
    pumped: bool,
    counters: Arc<Counters>,
}

type Halves = (pump::PumpIn, pump::PumpOut, pump::Finished);

impl Pipe {
    fn halves(&self) -> Result<Halves> {
        let spec = Spec { have: self.have.clone(), want: self.want.clone(), limit: self.limit };
        pump::pipe::new(spec, self.counters.clone())
    }
}

pub struct Runner {
    engine: Engine,
    linker: Linker<Ctx>,
    components: HashMap<String, Component>,
    opts: Options,
}

impl Runner {
    pub fn new(opts: Options) -> Result<Runner> {
        let engine = Engine::new(Config::new().wasm_component_model_async(true))?;
        let mut linker = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
        Ok(Runner { engine, linker, components: HashMap::new(), opts })
    }

    fn parallel(&self) -> bool {
        self.opts.parallel && self.opts.copy == Copy::Host
    }

    fn component(&mut self, name: &str) -> Result<Component> {
        if let Some(c) = self.components.get(name) {
            return Ok(c.clone());
        }
        // Cargo names `parse-log`'s library `parse_log.wasm`.
        let path = [name.to_string(), name.replace('-', "_")]
            .iter()
            .map(|n| self.opts.tools.join(format!("{n}.wasm")))
            .find(|p| p.exists())
            .with_context(|| format!("no tool named `{name}` in {}", self.opts.tools.display()))?;
        let component = self.compile(&std::fs::read(path)?)?;
        self.components.insert(name.to_string(), component.clone());
        Ok(component)
    }

    /// Compile a component, reusing a cached native build when possible.
    fn compile(&self, bytes: &[u8]) -> Result<Component> {
        let Some(dir) = &self.opts.cache else { return Ok(Component::new(&self.engine, bytes)?) };
        let mut h = DefaultHasher::new();
        (bytes, self.engine.precompile_compatibility_hash()).hash(&mut h);
        let file = dir.join(format!("{:016x}.cwasm", h.finish()));
        // SAFETY: the cache only holds files written below for a compatible engine.
        if let Ok(c) = unsafe { Component::deserialize_file(&self.engine, &file) } {
            return Ok(c);
        }
        let compiled = self.engine.precompile_component(bytes)?;
        let tmp = file.with_extension(format!("{}.tmp", std::process::id()));
        let _ = std::fs::create_dir_all(dir)
            .and_then(|_| std::fs::write(&tmp, &compiled))
            .and_then(|_| std::fs::rename(&tmp, &file));
        // SAFETY: just produced by `precompile_component` on this engine.
        Ok(unsafe { Component::deserialize(&self.engine, &compiled)? })
    }

    /// Resolve and type-check every step, before running anything.
    fn plan(&mut self, steps: &[Step]) -> Result<(Vec<Stage>, Vec<Pipe>)> {
        let (mut stages, mut pipes) = (Vec::<Stage>::new(), Vec::<Pipe>::new());
        // The stream flowing into the next step: its rows, and where it comes from.
        let mut flowing: Option<(Arc<Layout>, Input)> = None;
        let mut takes: Vec<u64> = Vec::new();
        for (i, step) in steps.iter().enumerate() {
            let (name, args) = match step {
                Step::Cat(path) => {
                    flowing = Some((Layout::of(&Type::U8)?, Input::Cat(path.clone())));
                    continue;
                }
                Step::Take(n) => {
                    takes.push(*n);
                    continue;
                }
                Step::Tool(name, args) => (name, args),
            };
            let component = self.component(name)?;
            let ty = component.component_type();
            let Some(ComponentItem::ComponentFunc(func)) = ty.get_export(&self.engine, "run").map(|r| r.ty) else {
                bail!("`{name}` has no `run` function");
            };
            let mut params: Vec<Type> = func.params().map(|(_, t)| t).collect();
            let output = match func.results().next() {
                Some(Type::Stream(s)) => Some(Layout::of(&s.ty().context("untyped streams aren't supported")?)?),
                _ => None,
            };
            let mut input = Input::None;
            if let Some((have, from)) = flowing.take() {
                let prev = steps[i - 1].label();
                let Some(Type::Stream(s)) = params.first() else {
                    bail!("`{name}` doesn't read a stream, but `{prev}` produces stream<{have}>");
                };
                let want = Layout::of(&s.ty().context("untyped streams aren't supported")?)?;
                pump::plan::project(&want, &have)
                    .map_err(|_| anyhow!("`{name}` expects stream<{want}>, but `{prev}` produces stream<{have}>"))?;
                params.remove(0);
                input = match from {
                    Input::Pipe(producer) => {
                        let limit = takes.iter().copied().min();
                        let label = [stages[producer].name.clone()]
                            .into_iter()
                            .chain(takes.drain(..).map(|n| format!("take {n}")))
                            .chain([name.clone()])
                            .collect::<Vec<_>>()
                            .join(" → ");
                        let pumped = self.parallel()
                            || limit.is_some()
                            || want != have
                            || (self.opts.copy == Copy::Host && !have.is_flat());
                        pipes.push(Pipe { label, have, want, limit, pumped, counters: Arc::default() });
                        stages[producer].output = Some(pipes.len() - 1);
                        Input::Pipe(pipes.len() - 1)
                    }
                    other => other,
                };
            }
            ensure!(
                i + 1 == steps.len() || output.is_some(),
                "`{name}` doesn't produce a stream, so nothing can follow it"
            );
            ensure!(i + 1 < steps.len() || output.is_none(), "`{name}` produces a stream, but nothing reads it");
            ensure!(args.len() <= params.len(), "`{name}` takes {} argument(s), got {}", params.len(), args.len());
            let args = params
                .iter()
                .enumerate()
                .map(|(k, t)| arg(t, args.get(k).map(String::as_str)))
                .collect::<Result<_>>()
                .with_context(|| format!("arguments to `{name}`"))?;
            stages.push(Stage { name: name.clone(), component, args, input, output: None });
            if let Some(out) = output {
                flowing = Some((out, Input::Pipe(stages.len() - 1)));
            }
        }
        Ok((stages, pipes))
    }

    fn store(&self, out: &Output) -> Store<Ctx> {
        let mut wasi = WasiCtx::builder();
        if self.opts.capture {
            wasi.stdout(out.stdout.clone()).stderr(out.stderr.clone());
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
        // still imported; give them empty instances.
        let mut linker = self.linker.clone();
        for (import, item) in stage.component.component_type().imports(&self.engine) {
            if let ComponentItem::ComponentInstance(inst) = item.ty
                && inst.exports(&self.engine).all(|(_, e)| matches!(e.ty, ComponentItem::Type(_)))
            {
                linker.instance(import)?;
            }
        }
        let instance = linker.instantiate_async(&mut *store, &stage.component).await?;
        instance.get_func(&mut *store, "run").context("no `run` export")
    }

    pub async fn run(&mut self, pipeline: &str) -> Result<Report> {
        let started = Instant::now();
        let (stages, pipes) = self.plan(&parse(pipeline)?)?;
        let out = Output::default();
        let (result, guest_memory, run) = if self.parallel() {
            self.run_parallel(&stages, &pipes, &out)?
        } else {
            self.run_serial(&stages, &pipes, &out).await?
        };

        let ms = |ns: &AtomicU64| ns.load(Relaxed) as f64 / 1e6;
        let pipes = pipes
            .iter()
            .filter(|p| p.pumped)
            .map(|p| {
                let c = &p.counters;
                PipeReport {
                    label: p.label.clone(),
                    elems: c.elems.load(Relaxed),
                    rows: c.rows.load(Relaxed),
                    batches: c.batches.load(Relaxed),
                    bytes: c.bytes.load(Relaxed),
                    reallocs: c.reallocs.load(Relaxed),
                    lift_ms: ms(&c.lift_ns),
                    lower_ms: ms(&c.lower_ns),
                }
            })
            .collect();
        Ok(Report {
            result,
            stdout: out.stdout.contents().to_vec(),
            stderr: out.stderr.contents().to_vec(),
            load: started.elapsed() - run,
            run,
            pipes,
            guest_memory,
            cat_bytes: out.cat_bytes.load(Relaxed),
            parallel: self.parallel(),
        })
    }

    /// Every component in one Store, driven by one task.
    async fn run_serial(&self, stages: &[Stage], pipes: &[Pipe], out: &Output) -> Result<(Val, u64, Duration)> {
        let mut store = self.store(out);
        let mut funcs = Vec::new();
        for stage in stages {
            funcs.push(self.instantiate(&mut store, stage).await?);
        }
        let mut halves: Vec<Option<Halves>> =
            pipes.iter().map(|p| p.pumped.then(|| p.halves()).transpose()).collect::<Result<_>>()?;
        let t0 = Instant::now();
        let result = store
            .run_concurrent(async |acc| -> Result<Val> {
                // Streams waiting for their consumer, by pipe.
                let mut streams: Vec<Option<StreamAny>> = pipes.iter().map(|_| None).collect();
                let mut last = None;
                for (stage, func) in stages.iter().zip(funcs) {
                    let input = match stage.input {
                        Input::Pipe(p) => {
                            let s = streams[p].take().context("pipe has no producer")?;
                            match halves[p].take() {
                                Some((pin, pout, _)) => {
                                    Some(acc.with(|mut a| pump::pump(a.as_context_mut(), s, pin, pout))?)
                                }
                                None => Some(s),
                            }
                        }
                        _ => out.open(acc, &stage.input)?,
                    };
                    match (call(acc, func, stage, input).await?, stage.output) {
                        (Val::Stream(s), Some(p)) => streams[p] = Some(s),
                        (v, _) => last = Some(v),
                    }
                }
                last.context("pipeline ended without a result")
            })
            .await??;
        Ok((result, store.data().mem.linear_bytes, t0.elapsed()))
    }

    /// Each component in its own Store on its own thread, with the host
    /// pumping rows between them: stages run truly in parallel.
    fn run_parallel(&self, stages: &[Stage], pipes: &[Pipe], out: &Output) -> Result<(Val, u64, Duration)> {
        let mut halves: Vec<_> =
            pipes.iter().map(|p| p.halves().map(|(i, o, f)| (Some(i), Some(o), Some(f)))).collect::<Result<_>>()?;
        let ready = std::sync::Barrier::new(stages.len());
        let t0 = Mutex::new(None::<Instant>);
        let outcomes: Vec<Result<(Option<Val>, u64)>> = std::thread::scope(|scope| {
            let handles: Vec<_> = stages
                .iter()
                .map(|stage| {
                    let pout = match stage.input {
                        Input::Pipe(p) => halves[p].1.take(),
                        _ => None,
                    };
                    let sink = stage.output.map(|p| (halves[p].0.take().unwrap(), halves[p].2.take().unwrap()));
                    let (ready, t0) = (&ready, &t0);
                    scope.spawn(move || -> Result<(Option<Val>, u64)> {
                        tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async move {
                            let mut store = self.store(out);
                            let func = self.instantiate(&mut store, stage).await;
                            ready.wait(); // start every stage together
                            let func = func?;
                            t0.lock().unwrap().get_or_insert_with(Instant::now);
                            let value = store
                                .run_concurrent(async move |acc| -> Result<Option<Val>> {
                                    let input = match pout {
                                        Some(pout) => {
                                            Some(acc.with(|mut a| pump::open_output(a.as_context_mut(), pout))?)
                                        }
                                        None => out.open(acc, &stage.input)?,
                                    };
                                    match (call(acc, func, stage, input).await?, sink) {
                                        (Val::Stream(s), Some((pin, finished))) => {
                                            acc.with(|mut a| pump::attach_input(a.as_context_mut(), s, pin))?;
                                            // Drive this Store until the producer is done, then let it
                                            // notice a cancellation and wind down before the Store goes.
                                            finished.await;
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
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err(anyhow!("a stage panicked")))).collect()
        });
        let run = t0.into_inner().unwrap().map(|t| t.elapsed()).unwrap_or_default();
        let (mut result, mut memory) = (None, 0);
        for (stage, outcome) in stages.iter().zip(outcomes) {
            let (value, mem) = outcome.with_context(|| format!("stage `{}`", stage.name))?;
            memory += mem;
            result = result.or(value);
        }
        Ok((result.context("pipeline ended without a result")?, memory, run))
    }
}

/// Where the components' output goes, plus `cat`'s byte count.
struct Output {
    stdout: MemoryOutputPipe,
    stderr: MemoryOutputPipe,
    cat_bytes: Arc<AtomicU64>,
}

impl Default for Output {
    fn default() -> Self {
        Output {
            stdout: MemoryOutputPipe::new(usize::MAX),
            stderr: MemoryOutputPipe::new(usize::MAX),
            cat_bytes: Arc::default(),
        }
    }
}

impl Output {
    /// The input stream for a stage that doesn't read from a pipe.
    fn open(&self, acc: &Accessor<Ctx>, input: &Input) -> Result<Option<StreamAny>> {
        match input {
            Input::Cat(path) => {
                Ok(Some(acc.with(|mut a| builtins::cat(a.as_context_mut(), path, self.cat_bytes.clone()))?))
            }
            _ => Ok(None),
        }
    }
}

/// Call a stage's `run` with its input stream (if any) and arguments.
async fn call(acc: &Accessor<Ctx>, func: Func, stage: &Stage, input: Option<StreamAny>) -> Result<Val> {
    let params: Vec<Val> = input.map(Val::Stream).into_iter().chain(stage.args.iter().cloned()).collect();
    let mut results = [Val::Bool(false)];
    func.call_concurrent(acc, &params, &mut results).await?;
    let [result] = results;
    Ok(result)
}

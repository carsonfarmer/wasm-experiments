//! Unix-style pipelines of WebAssembly components connected by typed
//! [WASI 0.3](https://wasi.dev) streams.
//!
//! ```text
//! wp 'parse-log | errors 500 | top-paths 10' < access.log
//! ```
//!
//! A *tool* is any component that exports a `run` function. If its first
//! parameter is a `stream<T>`, the previous tool feeds it (or, for
//! `stream<u8>` at the start of a pipeline, the input bytes do). If it returns
//! a `stream<U>`, the next tool reads it. Words after a tool's name are its
//! remaining arguments, written as [WAVE] values (strings may be bare).
//!
//! Every pipe is type-checked before anything runs. Wasmtime then checks
//! each pipe again, copies the rows between components and runs every tool
//! concurrently in one Store.
//!
//! [WAVE]: https://github.com/bytecodealliance/wasm-tools/tree/main/crates/wasm-wave

use anyhow::{Context as _, Result, bail, ensure};
use std::fmt;
use std::path::PathBuf;
use tokio::io::AsyncRead;
use wasmtime::component::types::{ComponentFunc, ComponentItem};
use wasmtime::component::wasm_wave::wasm::DisplayType;
use wasmtime::component::{Component, Func, Linker, ResourceTable, Type, Val};
use wasmtime::{AsContextMut, Cache, CacheConfig, Config, Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

mod bytes;

/// Finds tools and builds pipelines from them.
pub struct Wp {
    engine: Engine,
    linker: Linker<Ctx>,
    path: Vec<PathBuf>,
}

impl Wp {
    /// Look for tools (`NAME.wasm`) in the directories of `path`, in order.
    pub fn new(path: Vec<PathBuf>) -> Result<Wp> {
        let mut config = Config::new();
        // Compiled tools are cached on disk, so loading one is a quick read.
        config.wasm_component_model_async(true).cache(Some(Cache::new(CacheConfig::new())?));
        let engine = Engine::new(&config)?;
        let mut linker = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
        Ok(Wp { engine, linker, path })
    }

    /// Load the tool called `name`.
    pub fn tool(&self, name: &str) -> Result<Tool> {
        // Cargo names `parse-log`'s output `parse_log.wasm`.
        let files = [name.to_string(), name.replace('-', "_")].map(|n| format!("{n}.wasm"));
        let file = self
            .path
            .iter()
            .flat_map(|dir| files.iter().map(move |f| dir.join(f)))
            .find(|f| f.exists())
            .with_context(|| format!("no tool named `{name}`"))?;
        Tool::load(name, Component::from_file(&self.engine, &file)?)
    }

    /// Every tool on the path, by name.
    pub fn tools(&self) -> Result<Vec<Tool>> {
        let mut names: Vec<String> = Vec::new();
        for dir in &self.path {
            for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
                let file = entry?.path();
                if file.extension().is_some_and(|e| e == "wasm")
                    && let Some(stem) = file.file_stem()
                {
                    names.push(stem.to_string_lossy().replace('_', "-"));
                }
            }
        }
        names.sort();
        names.dedup();
        // Skip files that aren't tools (core modules, components without `run`).
        Ok(names.iter().filter_map(|n| self.tool(n).ok()).collect())
    }

    /// Parse and type-check a pipeline such as `gen 5 | show`.
    pub fn pipeline(&self, text: &str) -> Result<Pipeline<'_>> {
        let words = shlex::split(text).context("unbalanced quotes in pipeline")?;
        let mut stages = Vec::new();
        for stage in words.split(|w| w == "|") {
            let [name, args @ ..] = stage else { bail!("empty stage in pipeline") };
            let tool = self.tool(name)?;
            let args = tool.args(args).with_context(|| format!("arguments to `{name}`"))?;
            stages.push((tool, args));
        }
        check(&stages.iter().map(|(tool, _)| tool).collect::<Vec<_>>())?;
        Ok(Pipeline { wp: self, stages })
    }
}

/// A component exporting `run`.
pub struct Tool {
    pub name: String,
    component: Component,
    run: ComponentFunc,
    /// Interfaces the tool imports only for their types (like a shared row
    /// record), and the named types they hold.
    shared: Vec<String>,
    types: Vec<(String, Type)>,
}

impl Tool {
    fn load(name: &str, component: Component) -> Result<Tool> {
        let engine = component.engine();
        let ty = component.component_type();
        let Some(ComponentItem::ComponentFunc(run)) = ty.get_export(engine, "run").map(|e| e.ty) else {
            bail!("`{name}` doesn't export a `run` function");
        };
        let (mut shared, mut types) = (Vec::new(), Vec::new());
        for (import, item) in ty.imports(engine) {
            let ComponentItem::ComponentInstance(instance) = item.ty else { continue };
            let exports: Vec<_> = instance.exports(engine).map(|(n, e)| (n.to_string(), e.ty)).collect();
            if exports.iter().all(|(_, e)| matches!(e, ComponentItem::Type(_))) {
                shared.push(import.to_string());
                types.extend(exports.into_iter().filter_map(|(n, e)| match e {
                    ComponentItem::Type(t) => Some((n, t)),
                    _ => None,
                }));
            }
        }
        Ok(Tool { name: name.to_string(), component, run, shared, types })
    }

    /// The type of the rows this tool reads, if it reads a stream.
    pub fn input(&self) -> Option<Type> {
        stream_of(self.run.params().next().map(|(_, ty)| ty))
    }

    /// The type of the rows this tool writes, if it returns a stream.
    pub fn output(&self) -> Option<Type> {
        stream_of(self.run.results().next())
    }

    /// `ty` as WIT spells it, by name if the tool's WIT names it.
    pub fn wit(&self, ty: &Type) -> String {
        match (self.types.iter().find(|(_, t)| t == ty), ty) {
            (Some((name, _)), _) => name.clone(),
            (None, Type::Stream(s)) => format!("stream<{}>", s.ty().map_or("_".into(), |t| self.wit(&t))),
            (None, ty) => DisplayType(ty).to_string(), // wasm-wave prints all but streams
        }
    }

    /// Parse command-line words into the arguments after the input stream.
    fn args(&self, words: &[String]) -> Result<Vec<Val>> {
        let params: Vec<(&str, Type)> = self.run.params().skip(self.input().is_some() as usize).collect();
        ensure!(words.len() <= params.len(), "takes {} argument(s), got {}", params.len(), words.len());
        params
            .iter()
            .enumerate()
            .map(|(i, (name, ty))| match (words.get(i), ty) {
                (None, Type::Option(_)) => Ok(Val::Option(None)),
                (None, ty) => bail!("missing `{name}` ({})", self.wit(ty)),
                (Some(word), Type::String) => Ok(Val::String(word.clone())),
                (Some(word), ty) => Val::from_wave(ty, word)
                    .map_err(|_| anyhow::anyhow!("`{name}`: expected {}, got `{word}`", self.wit(ty))),
            })
            .collect()
    }

    async fn instantiate(&self, linker: &Linker<Ctx>, store: &mut Store<Ctx>) -> Result<Func> {
        let mut linker = linker.clone();
        for name in &self.shared {
            linker.instance(name)?; // types only, so an empty instance satisfies it
        }
        let instance = linker.instantiate_async(&mut *store, &self.component).await?;
        Ok(instance.get_func(&mut *store, "run").expect("checked when loaded"))
    }
}

fn stream_of(ty: Option<Type>) -> Option<Type> {
    match ty {
        Some(Type::Stream(s)) => s.ty(),
        _ => None,
    }
}

/// `name(param: type, ...) -> type`
impl fmt::Display for Tool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let params: Vec<String> = self.run.params().map(|(name, ty)| format!("{name}: {}", self.wit(&ty))).collect();
        write!(f, "{}({})", self.name, params.join(", "))?;
        self.run.results().try_for_each(|ty| write!(f, " -> {}", self.wit(&ty)))
    }
}

/// Check that each tool reads exactly what the one before it writes.
fn check(tools: &[&Tool]) -> Result<()> {
    let (first, last) = (tools[0], tools[tools.len() - 1]);
    if let Some(want) = first.input() {
        ensure!(
            want == Type::U8,
            "`{}` reads stream<{}>, but a pipeline can only start with bytes",
            first.name,
            first.wit(&want)
        );
    }
    for pair in tools.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        match (a.output(), b.input()) {
            (Some(have), Some(want)) if have == want => {}
            (Some(have), Some(want)) => {
                let (mut h, mut w) = (a.wit(&have), b.wit(&want));
                if h == w {
                    // Same name, different types: show the structure.
                    (h, w) = (DisplayType(&have).to_string(), DisplayType(&want).to_string());
                }
                bail!("`{}` reads stream<{w}>, but `{}` produces stream<{h}>", b.name, a.name)
            }
            (Some(have), None) => {
                bail!("`{}` produces stream<{}>, but `{}` doesn't read a stream", a.name, a.wit(&have), b.name)
            }
            (None, _) => bail!("`{}` doesn't produce a stream, so nothing can follow it", a.name),
        }
    }
    if let Some(have) = last.output() {
        bail!("`{}` produces stream<{}>, but nothing reads it", last.name, last.wit(&have));
    }
    Ok(())
}

/// A type-checked pipeline, ready to run.
pub struct Pipeline<'a> {
    wp: &'a Wp,
    stages: Vec<(Tool, Vec<Val>)>,
}

impl Pipeline<'_> {
    pub fn tools(&self) -> impl Iterator<Item = &Tool> {
        self.stages.iter().map(|(tool, _)| tool)
    }

    /// Run every tool to completion in one Store, sharing `wasi`. `input`
    /// feeds the first tool if it reads bytes. Returns the last tool's
    /// result, if it has one.
    pub async fn run(&self, wasi: WasiCtx, input: impl AsyncRead + Send + Unpin + 'static) -> Result<Option<Val>> {
        let mut store = Store::new(&self.wp.engine, Ctx { wasi, table: ResourceTable::new() });
        let mut funcs = Vec::new();
        for (tool, _) in &self.stages {
            funcs.push(tool.instantiate(&self.wp.linker, &mut store).await?);
        }
        let first_reads_bytes = self.stages[0].0.input().is_some();
        store
            .run_concurrent(async |acc| {
                let mut stream = None;
                if first_reads_bytes {
                    stream = Some(acc.with(|mut a| bytes::stream(a.as_context_mut(), input))?);
                }
                let mut result = None;
                for ((tool, args), func) in self.stages.iter().zip(funcs) {
                    let params: Vec<Val> =
                        stream.take().map(Val::Stream).into_iter().chain(args.iter().cloned()).collect();
                    let mut results = vec![Val::Bool(false); tool.run.results().len()];
                    func.call_concurrent(acc, &params, &mut results).await?;
                    match results.pop() {
                        Some(Val::Stream(s)) => stream = Some(s),
                        last => result = last,
                    }
                }
                Ok(result)
            })
            .await?
    }
}

struct Ctx {
    wasi: WasiCtx,
    table: ResourceTable,
}

impl WasiView for Ctx {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView { ctx: &mut self.wasi, table: &mut self.table }
    }
}

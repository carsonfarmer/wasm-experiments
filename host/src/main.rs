//! `wp`: run a pipeline of WebAssembly components connected by typed streams.
//!
//!     wp 'gen 5 | show'            two components, one stream<commit>
//!     wp 'gen | take 10 | show'    `take` is a host built-in; `gen` alone runs forever
//!     wp --time 'gen 1000000 | count'
//!
//! Every stage after the first gets the previous stage's stream as its first
//! argument; any remaining words become its other arguments. The host checks
//! that each pipe's element types line up before running anything.

mod dynval;
mod take;

use anyhow::{Context as _, Result, bail};
use std::path::PathBuf;
use std::time::Instant;
use wasmtime::component::types::ComponentItem;
use wasmtime::component::{Component, Func, Linker, ResourceTable, Type, Val};
use wasmtime::{AsContextMut, Config, Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

struct Ctx {
    wasi: WasiCtx,
    table: ResourceTable,
}

impl WasiView for Ctx {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView { ctx: &mut self.wasi, table: &mut self.table }
    }
}

enum Stage {
    Tool { name: String, args: Vec<String>, func: Option<Func> },
    Take(u64),
}

impl Stage {
    fn label(&self) -> String {
        match self {
            Stage::Tool { name, .. } => format!("`{name}`"),
            Stage::Take(n) => format!("`take {n}`"),
        }
    }
}

fn parse(pipeline: &str) -> Result<Vec<Stage>> {
    pipeline
        .split('|')
        .map(|part| {
            let mut words = part.split_whitespace().map(String::from);
            let name = words.next().context("empty stage in pipeline")?;
            let args: Vec<String> = words.collect();
            Ok(match name.as_str() {
                "take" => match args.as_slice() {
                    [n] => Stage::Take(n.parse().context("take expects a number")?),
                    _ => bail!("usage: take N"),
                },
                _ => Stage::Tool { name, args, func: None },
            })
        })
        .collect()
}

/// Convert a command-line word into a value of the parameter's type.
fn arg(ty: &Type, word: Option<&str>) -> Result<Val> {
    let Some(w) = word else {
        return match ty {
            Type::Option(_) => Ok(Val::Option(None)),
            _ => bail!("missing argument of type {}", dynval::describe(ty)),
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
        other => bail!("can't pass {} on the command line", dynval::describe(other)),
    })
}

fn stream_elem(ty: &Type) -> Option<Option<Type>> {
    match ty {
        Type::Stream(s) => Some(s.ty()),
        _ => None,
    }
}

fn elem_str(t: &Option<Type>) -> String {
    match t {
        Some(t) => format!("stream<{}>", dynval::describe(t)),
        None => "stream".into(),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut time = false;
    let mut tools = PathBuf::from(
        std::env::var("WP_TOOLS").unwrap_or_else(|_| "target/wasm32-wasip2/release".into()),
    );
    let mut words = Vec::new();
    let mut argv = std::env::args().skip(1);
    while let Some(a) = argv.next() {
        match a.as_str() {
            "--time" => time = true,
            "--tools" => tools = argv.next().context("--tools needs a directory")?.into(),
            _ => words.push(a),
        }
    }
    if words.is_empty() {
        eprintln!("usage: wp [--time] [--tools DIR] 'gen 5 | take 3 | show'");
        std::process::exit(2);
    }
    let mut stages = parse(&words.join(" "))?;
    if stages.len() < 2 {
        bail!("a pipeline needs at least a producer and a consumer");
    }
    if !matches!(stages.first(), Some(Stage::Tool { .. })) || !matches!(stages.last(), Some(Stage::Tool { .. })) {
        bail!("pipelines must start and end with a component");
    }

    let mut config = Config::new();
    config.wasm_component_model_async(true);
    let engine = Engine::new(&config)?;
    let mut linker = Linker::<Ctx>::new(&engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    let wasi = WasiCtx::builder().inherit_stdio().build();
    let mut store = Store::new(&engine, Ctx { wasi, table: ResourceTable::new() });

    // Load every component and find its `run` export.
    let started = Instant::now();
    for stage in &mut stages {
        if let Stage::Tool { name, func, .. } = stage {
            let path = tools.join(format!("{name}.wasm"));
            let component = Component::from_file(&engine, &path)
                .map_err(anyhow::Error::from)
                .with_context(|| format!("loading {}", path.display()))?;
            // Interfaces that only declare types (like the shared `commit`
            // record) are still imported as instances; give them empty ones.
            let mut l = linker.clone();
            for (import, item) in component.component_type().imports(&engine) {
                if let ComponentItem::ComponentInstance(inst) = item.ty {
                    let types_only = inst
                        .exports(&engine)
                        .all(|(_, e)| matches!(e.ty, ComponentItem::Type(_)));
                    if types_only {
                        l.instance(import)?;
                    }
                }
            }
            let instance = l.instantiate_async(&mut store, &component).await?;
            *func = Some(
                instance
                    .get_func(&mut store, "run")
                    .with_context(|| format!("`{name}` has no `run` export"))?,
            );
        }
    }
    let load_time = started.elapsed();

    // Type-check every pipe before running anything.
    let mut plan: Vec<(Vec<Val>, Option<Type>)> = Vec::new(); // (extra args, elem type flowing out)
    let mut flowing: Option<Option<Type>> = None;
    for (i, stage) in stages.iter().enumerate() {
        match stage {
            Stage::Take(_) => {
                let elem = flowing.clone().expect("take is never first");
                plan.push((vec![], elem));
            }
            Stage::Tool { name, args, func } => {
                let ty = func.unwrap().ty(&store);
                let mut params: Vec<Type> = ty.params().map(|(_, t)| t).collect();
                if let Some(expected) = &flowing {
                    let got = params.first().and_then(stream_elem);
                    let ok = match (&got, expected) {
                        (Some(a), b) => elem_str(a) == elem_str(b),
                        (None, _) => false,
                    };
                    if !ok {
                        bail!(
                            "{} expects {}, but {} produces {}",
                            stage.label(),
                            params.first().map(dynval::describe).unwrap_or_else(|| "no input".into()),
                            stages[i - 1].label(),
                            elem_str(expected),
                        );
                    }
                    params.remove(0);
                }
                if args.len() > params.len() {
                    bail!("`{name}` takes {} argument(s), got {}", params.len(), args.len());
                }
                let vals = params
                    .iter()
                    .enumerate()
                    .map(|(k, t)| arg(t, args.get(k).map(|s| s.as_str())))
                    .collect::<Result<Vec<_>>>()
                    .with_context(|| format!("arguments to `{name}`"))?;
                let result = ty.results().next();
                let out = result.as_ref().and_then(stream_elem);
                if i + 1 < stages.len() && out.is_none() {
                    bail!("`{name}` doesn't produce a stream, so nothing can follow it");
                }
                plan.push((vals, out.clone().flatten()));
                flowing = out;
            }
        }
    }

    // Run: call each stage's `run`, handing the previous stream to the next.
    let run_started = Instant::now();
    let outcome = store
        .run_concurrent(async |acc| -> Result<Val> {
            let mut stream: Option<Val> = None;
            for (i, stage) in stages.iter().enumerate() {
                let (extra, _) = &plan[i];
                match stage {
                    Stage::Take(n) => {
                        let Some(Val::Stream(s)) = stream.take() else { unreachable!() };
                        let elem = plan[i].1.clone().context("take needs a typed stream")?;
                        let layout = dynval::layout(&elem)?;
                        let out = acc.with(|mut a| take::take(a.as_context_mut(), s, *n, layout))?;
                        stream = Some(Val::Stream(out));
                    }
                    Stage::Tool { func, .. } => {
                        let mut params: Vec<Val> = stream.take().into_iter().collect();
                        params.extend(extra.iter().cloned());
                        let mut results = vec![Val::Bool(false)];
                        func.unwrap().call_concurrent(acc, &params, &mut results).await?;
                        stream = results.pop();
                    }
                }
            }
            stream.context("last stage returned nothing")
        })
        .await??;
    let run_time = run_started.elapsed();

    if time {
        let rows = match outcome {
            Val::U64(n) => n,
            _ => 0,
        };
        let secs = run_time.as_secs_f64();
        eprintln!(
            "load {:.1} ms · run {:.1} ms · {rows} rows · {:.2} M rows/s",
            load_time.as_secs_f64() * 1e3,
            secs * 1e3,
            rows as f64 / secs / 1e6
        );
    }
    Ok(())
}

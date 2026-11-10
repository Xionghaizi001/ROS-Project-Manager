//! Explicit worker lifecycle; clients may disconnect without ending tasks.
use super::{context, load, Global};
use crate::i18n::tr;
use anyhow::{bail, Context as _, Result};
use clap::Subcommand;
use rospm_core::{
    execution::{Event, Interaction, Stream},
    manifest::MANIFEST_FILE,
    mirror,
    probe::Host,
    provider,
    worker::*,
};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    os::unix::{
        fs::{FileTypeExt, MetadataExt},
        net::UnixStream,
    },
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

#[derive(Subcommand)]
pub enum TaskCommand {
    #[command(about = tr("worker_cli.submit"))]
    Submit {
        #[arg(value_parser = ["apply", "apply_locked", "build", "uninstall", "check", "verify", "deps"])]
        operation: String,
        #[arg(long, help = tr("worker_cli.id"))]
        id: String,
    },
    #[command(about = tr("worker_cli.status"))]
    Status { id: String },
    #[command(about = tr("worker_cli.logs"))]
    Logs {
        id: String,
        #[arg(long, default_value_t = 0)]
        cursor: u64,
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..))]
        limit: u32,
    },
    #[command(about = tr("worker_cli.confirm"))]
    Confirm {
        id: String,
        #[arg(long)]
        step: u32,
        #[arg(value_parser = ["approve", "reject", "cancel"])]
        response: String,
    },
    #[command(about = tr("worker_cli.cancel"))]
    Cancel { id: String },
}

fn exchange(stream: &mut BufReader<UnixStream>, message: Message) -> Result<Message> {
    let bytes = Frame::new(message).encode()?;
    anyhow::ensure!(
        bytes.len() <= MAX_FRAME_BYTES,
        "{}",
        tr("worker_cli.frame_limit")
    );
    stream.get_mut().write_all(&bytes)?;
    let mut reply = Vec::new();
    stream
        .take(MAX_FRAME_BYTES as u64 + 1)
        .read_until(b'\n', &mut reply)?;
    let reply = Frame::decode(&reply)?.message;
    if let Message::Error { code, message } = &reply {
        bail!("{code}: {message}");
    }
    Ok(reply)
}

pub fn client(global: &Global, runtime: &Path, command: TaskCommand) -> Result<()> {
    let request = match command {
        TaskCommand::Submit { operation, id } => {
            let loaded = load(global)?;
            anyhow::ensure!(
                loaded.path.canonicalize()? == loaded.root.join(MANIFEST_FILE),
                "{}",
                tr("worker_cli.standard_manifest")
            );
            Message::Submit {
                request_id: id.clone(),
                task_id: id,
                workspace: loaded.root.to_str().context(tr("worker_cli.utf8"))?.into(),
                operation: serde_json::from_value(serde_json::Value::String(operation))?,
            }
        }
        TaskCommand::Status { id } => Message::Snapshot { task_id: id },
        TaskCommand::Logs { id, cursor, limit } => Message::Logs {
            task_id: id,
            cursor,
            limit,
        },
        TaskCommand::Cancel { id } => Message::Cancel { task_id: id },
        TaskCommand::Confirm { id, step, response } => Message::Confirm {
            task_id: id,
            step,
            response: serde_json::from_value(serde_json::Value::String(response))?,
        },
    };
    let stream = match connect(runtime) {
        Ok(stream) => stream,
        Err(error)
            if error.downcast_ref::<std::io::Error>().is_some_and(|e| {
                matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                )
            }) =>
        {
            let root = load(global)?.root.join(".rospm/worker/tasks");
            anyhow::ensure!(root.is_dir(), "{}", tr("worker_cli.unavailable"));
            let store = TaskStore::open(root)?;
            let reply = match request {
                Message::Snapshot { task_id } => {
                    let snapshot = store.load_snapshot(&task_id)?;
                    anyhow::ensure!(
                        snapshot.state.is_terminal(),
                        "{}",
                        tr("worker_cli.unavailable")
                    );
                    Message::State { snapshot }
                }
                Message::Logs {
                    task_id,
                    cursor,
                    limit,
                } => store.log_chunk(&task_id, cursor, limit)?,
                _ => return Err(error),
            };
            println!("{}", serde_json::to_string_pretty(&Frame::new(reply))?);
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut stream = BufReader::new(stream);
    anyhow::ensure!(
        matches!(
            exchange(
                &mut stream,
                Message::Hello {
                    client: "rospm-cli".into()
                }
            )?,
            Message::Hello { .. }
        ),
        "{}",
        tr("worker_cli.expected_hello")
    );
    println!(
        "{}",
        serde_json::to_string_pretty(&Frame::new(exchange(&mut stream, request)?))?
    );
    Ok(())
}

fn connect(runtime: &Path) -> Result<UnixStream> {
    let metadata = std::fs::symlink_metadata(runtime)?;
    anyhow::ensure!(
        metadata.is_dir()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o077 == 0,
        "{}",
        tr("worker_cli.private_runtime")
    );
    Ok(UnixStream::connect(runtime.join("worker.sock"))?)
}

static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: libc::c_int) {
    STOP.store(true, Ordering::Release);
}

pub fn serve(global: &Global, runtime: &Path) -> Result<()> {
    let loaded = load(global)?;
    anyhow::ensure!(
        loaded.path.canonicalize()? == loaded.root.join(MANIFEST_FILE),
        "{}",
        tr("worker_cli.standard_manifest")
    );
    // The project lock prevents two endpoints from recovering the same task store.
    let data = loaded.root.join(".rospm/worker");
    let store = TaskStore::open(data.join("tasks"))?;
    let _owner = WorkspaceLock::try_acquire(&data, &loaded.root)?
        .context(tr("worker_cli.already_running"))?;
    TaskStore::open(runtime)?;
    let runtime = runtime.canonicalize()?;
    let _endpoint_owner =
        WorkspaceLock::try_acquire(&runtime, &runtime)?.context(tr("worker_cli.runtime_in_use"))?;
    let socket = runtime.join("worker.sock");
    if let Ok(meta) = std::fs::symlink_metadata(&socket) {
        anyhow::ensure!(
            meta.file_type().is_socket(),
            "{}",
            tr("worker_cli.not_socket")
        );
        anyhow::ensure!(
            UnixStream::connect(&socket).is_err(),
            "{}",
            tr("worker_cli.active_socket")
        );
        std::fs::remove_file(&socket)?;
    }
    let endpoint = SocketEndpoint::bind(&runtime)?;
    // A previous process may have left children running; never replay its work.
    store.recover_active_tasks(RecoveryCause::HostFailure)?;
    let runner = TaskRunner::new(store.clone(), data.join("leases"));
    let mut tasks: HashMap<String, TaskHandle> = HashMap::new();
    unsafe {
        libc::signal(libc::SIGINT, stop as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, stop as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
    }
    let result = endpoint.serve_tasks(&STOP, &store, |_, message| {
        let finished: Vec<_> = tasks
            .iter()
            .filter(|(_, h)| h.is_finished())
            .map(|(id, _)| id.clone())
            .collect();
        for id in finished {
            if let Err(error) = tasks.remove(&id).unwrap().join() {
                eprintln!("{id}: {error}");
            }
        }
        match message {
            Message::Submit {
                task_id,
                workspace,
                operation,
                ..
            } => {
                if Path::new(&workspace) != loaded.root {
                    return Err("workspace does not match worker".into());
                }
                if tasks.len() >= 32 {
                    return Err("worker task capacity reached".into());
                }
                let root = loaded.root.clone();
                let id = task_id.clone();
                let handle = runner
                    .start_with(&task_id, &workspace, move |interaction| {
                        execute(&root, &id, operation, interaction).map_err(|e| {
                            let description = super::describe(&e);
                            e.downcast::<rospm_core::Error>().unwrap_or_else(|_| {
                                rospm_core::Error::Host(rospm_core::Message::text(description))
                            })
                        })
                    })
                    .map_err(|e| e.to_string())?;
                tasks.insert(task_id, handle);
            }
            Message::Confirm {
                task_id,
                step,
                response,
            } => {
                tasks
                    .get(&task_id)
                    .ok_or("task is not active")?
                    .respond_at(step, response)
                    .map_err(|e| e.to_string())?;
            }
            Message::Cancel { task_id } => {
                tasks.get(&task_id).ok_or("task is not active")?.cancel()
            }
            _ => return Err("unsupported request".into()),
        }
        Ok(())
    });
    for handle in tasks.values() {
        handle.cancel();
    }
    for (id, handle) in tasks {
        if let Err(error) = handle.join() {
            eprintln!("{id}: {error}");
        }
    }
    result?;
    Ok(())
}

fn execute(
    root: &Path,
    id: &str,
    operation: Operation,
    interaction: &dyn Interaction,
) -> Result<()> {
    let loaded = load(&Global {
        manifest: Some(root.join(MANIFEST_FILE)),
        json: true,
    })?;
    let _leases = loaded.workspace_leases()?;
    let locked = operation == Operation::ApplyLocked;
    let lock = if locked || operation == Operation::Verify {
        Some(super::apply::load_locked_lock(&loaded)?)
    } else {
        None
    };
    let host = Host::probe();
    let ctx = context(
        &loaded,
        &host,
        locked,
        lock.as_ref(),
        false,
        mirror::OFFICIAL,
    )?;
    let provider = provider::for_manifest(&loaded.manifest);
    let plan = match operation {
        Operation::Apply | Operation::ApplyLocked => provider.plan_apply(&ctx)?,
        Operation::Build => provider.plan_build(&ctx, None, false)?,
        Operation::Uninstall => {
            let plan = provider.plan_uninstall(&ctx, false)?;
            rospm_core::confirmation::uninstall(id, &plan, false, interaction, |plan| {
                plan.perform_with(id, interaction)
            })?;
            return Ok(());
        }
        Operation::Check | Operation::Verify | Operation::Deps => {
            let report = match operation {
                Operation::Check => provider.probe_env(&ctx)?,
                Operation::Verify => provider.verify(&ctx, lock.as_ref().unwrap())?,
                _ => provider.probe_deps(&ctx)?.environment,
            };
            let bytes = serde_json::to_vec(&report)?;
            interaction.event(Event::Log {
                task: id,
                step: 0,
                stream: Stream::Stdout,
                bytes: &bytes,
            });
            anyhow::ensure!(report.ready(), "{}", tr("worker_cli.check_failed"));
            return Ok(());
        }
    };
    plan.perform_with(id, interaction)?;
    Ok(())
}

use std::path::Path;

use anyhow::{Context, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rospm_core::run::{RunExit, RunRequest, RunSignal, RunStatus, Target};
use rospm_core::worker::Operation;

use crate::{dashboard::Dashboard, pages::Entry, Text};

pub trait RunBackend {
    fn start(&mut self, root: &Path, request: RunRequest, background: bool) -> Result<()>;
    fn foreground(&mut self) -> Vec<RunStatus>;
    fn background(&mut self, root: &Path) -> Result<Vec<RunStatus>>;
    fn history(&mut self) -> Vec<RunStatus>;
    fn signal(&mut self, root: &Path, process: &RunStatus, signal: RunSignal) -> Result<()>;
    fn close_foreground(&mut self);
    fn shell(&mut self, root: &Path, manifest: &Path) -> Result<()>;

    fn has_task_worker(&self) -> bool {
        false
    }

    fn task(
        &mut self,
        _root: &Path,
        _manifest: Option<&Path>,
        _operation: Operation,
    ) -> Result<TaskOutcome> {
        anyhow::bail!("task worker unavailable")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskOutcome {
    Completed,
    Detached,
}

pub(crate) enum Page {
    Form {
        target: Target,
        args: String,
        venv: String,
        edit: Option<usize>,
        start: Option<bool>,
        error: Option<String>,
    },
    Processes {
        list: Vec<RunStatus>,
        selected: Option<RunStatus>,
        signal: Option<RunSignal>,
        error: Option<String>,
    },
}

impl Page {
    pub fn form(target: Target) -> Self {
        Self::Form {
            target,
            args: String::new(),
            venv: String::new(),
            edit: None,
            start: None,
            error: None,
        }
    }

    pub fn processes(list: Vec<RunStatus>, error: Option<String>) -> Self {
        Self::Processes {
            list,
            selected: None,
            signal: None,
            error,
        }
    }

    pub fn title(&self, text: &Text) -> String {
        match self {
            Self::Form { target, .. } => format!("{}/{}", target.package, target.name),
            Self::Processes { selected, .. } => selected.as_ref().map_or_else(
                || text.run_processes.clone(),
                |process| format!("{} / {}", text.run_processes, process.target),
            ),
        }
    }

    pub fn rows(&self, text: &Text) -> Vec<Entry> {
        let (mut rows, error) = match self {
            Self::Form {
                args,
                venv,
                edit,
                error,
                ..
            } => {
                let mut rows = vec![
                    Entry::link(&text.run_foreground),
                    Entry::link(&text.run_background),
                    Entry::link(&text.run_args),
                    Entry::link(&text.run_venv),
                ];
                rows[2].cells[2] = args.clone();
                rows[3].cells[2] = if venv.is_empty() && *edit != Some(3) {
                    text.run_venv_help.clone()
                } else {
                    venv.clone()
                };
                if let Some(index) = *edit {
                    rows[index].cells[2].push('▏');
                }
                (rows, error)
            }
            Self::Processes {
                list,
                selected,
                error,
                ..
            } => {
                let mut rows = Vec::new();
                if let Some(process) = selected {
                    rows.push(process_row(process, text));
                    rows[0].selectable = false;
                    if controllable(process) {
                        rows.extend([
                            Entry::link(&text.run_terminate),
                            Entry::link(&text.run_kill),
                            Entry::link(if process.suspended {
                                &text.run_resume
                            } else {
                                &text.run_suspend
                            }),
                        ]);
                    }
                    if process.output_fd.is_some() {
                        rows.push(Entry::link(&text.logs));
                    }
                } else {
                    let (active, history) = list.split_at(active_count(list));
                    for (title, entries, empty) in [
                        (&text.run_current, active, &text.run_idle),
                        (&text.run_history, history, &text.run_no_history),
                    ] {
                        let mut heading = Entry::value(
                            title,
                            if entries.is_empty() {
                                empty.as_str()
                            } else {
                                ""
                            },
                        );
                        heading.heading = true;
                        rows.push(heading);
                        rows.extend(entries.iter().map(|process| process_row(process, text)));
                    }
                }
                (rows, error)
            }
        };
        if let Some(error) = error {
            rows.push(Entry::value(&text.status, error));
        }
        rows
    }

    pub fn activate(&mut self, index: usize) -> Option<usize> {
        match self {
            Self::Form { edit, start, .. } => match index {
                0 | 1 => *start = Some(index == 1),
                2 | 3 => *edit = Some(index),
                _ => {}
            },
            Self::Processes {
                list,
                selected,
                signal,
                error,
            } => {
                if let Some(process) = selected.as_ref().filter(|process| controllable(process)) {
                    *signal = match index {
                        1 => Some(RunSignal::Terminate),
                        2 => Some(RunSignal::Kill),
                        3 if process.suspended => Some(RunSignal::Resume),
                        3 => Some(RunSignal::Suspend),
                        _ => None,
                    };
                } else if selected.is_none() {
                    let Some(process) = process_at(list, index) else {
                        return None;
                    };
                    *selected = Some(process.clone());
                    *error = None;
                    return (controllable(process) || process.output_fd.is_some()).then_some(1);
                }
            }
        }
        Some(index)
    }

    pub fn log_at(&self, index: usize) -> Option<&RunStatus> {
        let Self::Processes { list, selected, .. } = self else {
            return None;
        };
        let process = match selected {
            Some(process) if index == if controllable(process) { 4 } else { 1 } => process,
            None => process_at(list, index).filter(|process| process.exit.is_some())?,
            _ => return None,
        };
        process.output_fd.map(|_| process)
    }

    pub fn is_processes(&self) -> bool {
        matches!(self, Self::Processes { .. })
    }

    pub fn edit_key(&mut self, key: KeyEvent) -> bool {
        let Self::Form {
            args,
            venv,
            edit: Some(index),
            ..
        } = self
        else {
            return false;
        };
        let input = if *index == 2 { args } else { venv };
        match key.code {
            KeyCode::Esc | KeyCode::Enter => {
                if let Self::Form { edit, .. } = self {
                    *edit = None;
                }
            }
            KeyCode::Backspace => {
                input.pop();
            }
            KeyCode::Char(ch)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                input.push(ch);
            }
            _ => {}
        }
        true
    }

    pub fn back(&mut self) -> bool {
        match self {
            Self::Processes { selected, .. } => selected.take().is_some(),
            Self::Form { edit, .. } => edit.take().is_some(),
        }
    }

    // Return true when a launch was accepted and the target list should reopen.
    pub fn dispatch(
        &mut self,
        backend: &mut dyn RunBackend,
        project: &Dashboard,
        verbosity: u8,
        text: &Text,
    ) -> bool {
        match self {
            Self::Form {
                target,
                args,
                venv,
                start,
                error,
                ..
            } => {
                let Some(background) = start.take() else {
                    return false;
                };
                let result = (|| {
                    let args = shlex::split(args).with_context(|| text.run_bad_args.clone())?;
                    let venv = venv.trim();
                    let cwd = std::env::current_dir()?;
                    backend.start(
                        &project.root,
                        RunRequest {
                            manifest: cwd.join(&project.manifest),
                            kind: target.kind,
                            package: target.package.clone(),
                            name: target.name.clone(),
                            args,
                            venv: venv == "--venv",
                            venv_path: (!venv.is_empty() && venv != "--venv")
                                .then(|| cwd.join(venv)),
                            verbosity,
                        },
                        background,
                    )
                })();
                *error = result.as_ref().err().map(|error| format!("{error:#}"));
                result.is_ok()
            }
            Self::Processes {
                selected,
                signal,
                error,
                ..
            } => {
                if let (Some(process), Some(signal)) = (selected, signal.take()) {
                    *error = backend
                        .signal(&project.root, process, signal)
                        .err()
                        .map(|error| format!("{error:#}"));
                }
                false
            }
        }
    }

    pub fn refresh(
        &mut self,
        list: &[RunStatus],
        query_error: Option<&str>,
        focus: &mut Option<usize>,
        text: &Text,
    ) {
        let Self::Processes {
            list: current,
            selected,
            error,
            ..
        } = self
        else {
            return;
        };
        if let Some(process) = selected {
            if let Some(updated) = list.iter().find(|item| item.id == process.id) {
                if controllable(process) != controllable(updated) {
                    *focus = Some(1);
                }
                *process = updated.clone();
            } else if query_error.is_none() || !process.background {
                *selected = None;
                *focus = first_process(list);
                *error = Some(text.run_gone.clone());
            }
        } else {
            let focused = focus.and_then(|index| process_at(current, index));
            *focus = focused
                .and_then(|process| list.iter().position(|item| item.id == process.id))
                .map(|index| index + if index < active_count(list) { 1 } else { 2 })
                .or_else(|| first_process(list));
        }
        current.clear();
        current.extend_from_slice(list);
        if let Some(query_error) = query_error {
            *error = Some(query_error.into());
        }
    }
}

fn active_count(list: &[RunStatus]) -> usize {
    list.iter().take_while(|item| item.exit.is_none()).count()
}

fn process_at(list: &[RunStatus], row: usize) -> Option<&RunStatus> {
    let boundary = active_count(list) + 1;
    if row == 0 || row == boundary {
        return None;
    }
    list.get(row - if row < boundary { 1 } else { 2 })
}

fn first_process(list: &[RunStatus]) -> Option<usize> {
    list.first()
        .map(|process| if process.exit.is_none() { 1 } else { 2 })
}

fn controllable(process: &RunStatus) -> bool {
    process.exit.is_none() && process.pid.is_some()
}

fn process_row(process: &RunStatus, text: &Text) -> Entry {
    let mut row = Entry::link(&process.target);
    row.cells[1] = if process.task && process.exit.is_none() {
        text.installing.clone()
    } else if process.exit.is_some() {
        text.run_exited.clone()
    } else if process.shell {
        text.shell_detached.clone()
    } else if process.background {
        text.run_background.clone()
    } else {
        text.run_foreground.clone()
    }
    .clone();
    row.cells[2] = match (process.exit, process.pid) {
        (Some(RunExit::Code(code)), _) => format!("{} {code}", text.run_exit_code),
        (Some(RunExit::Signal(signal)), _) => format!("{} {signal}", text.run_exit_signal),
        (Some(RunExit::Unknown), _) => text.run_exit_unknown.clone(),
        (None, Some(pid)) => format!(
            "PID {pid} · {}",
            if process.suspended {
                &text.run_suspended
            } else {
                &text.running
            }
        ),
        (None, None) => text.run_starting.clone(),
    };
    row
}

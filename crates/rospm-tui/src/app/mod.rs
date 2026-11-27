mod footer;

use std::{
    cell::Cell,
    io::{self, Write},
    path::Path,
    time::{Duration, Instant},
};

use anyhow::Result;
use crossterm::{
    event::{self, EnableMouseCapture, Event, KeyCode, KeyModifiers, MouseButton, MouseEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen},
};
use ratatui::Terminal;
use rospm_core::worker::WorkspaceLock;

use crate::{
    actions::{
        execute_pending_confirmation, install, request_action, request_compile, task_backend,
        PendingConfirmation,
    },
    dashboard, log_view, logs,
    navigation::{
        activate, back_run, first_selectable, lock_project, move_focus, open_project, tab_at,
    },
    pages, runs, table,
    terminal::{draw_frame, leave_screen},
    view, Action, InstallPage, RunBackend, Text,
};

use self::footer::activate_footer;

pub(super) const PROCESS_KEY: char = 'p';
pub(super) const LOG_KEY: char = 'l';

#[allow(clippy::too_many_arguments)] // Callbacks and the workspace lock remain owned by the terminal session.
pub(super) fn loop_ui<B: ratatui::backend::Backend + Write>(
    terminal: &mut Terminal<B>,
    drawing: &Cell<bool>,
    manifest: Option<&Path>,
    mut dashboard: Option<dashboard::Dashboard>,
    text: &Text,
    install_ros: &mut impl FnMut() -> Result<()>,
    install_components: &mut impl FnMut() -> Result<()>,
    build: &mut impl FnMut(bool) -> Result<()>,
    action: &mut impl FnMut(Action) -> Result<()>,
    runs: &mut Option<(&mut dyn RunBackend, u8)>,
    project_lock: &mut Option<WorkspaceLock>,
) -> Result<()> {
    let mut active_manifest = manifest.map(Path::to_path_buf);
    let mut browser: Option<logs::Browser> = None;
    let mut selected = 0usize;
    let mut install_page: Option<InstallPage> = None;
    let mut scroll = 0usize;
    let mut focused_row = first_selectable(dashboard.as_ref(), text, selected, None);
    let mut last_runtime_refresh = Instant::now();
    let mut last_process_refresh = None::<Instant>;
    let mut processes = Vec::new();
    let mut active_processes = 0;
    let mut process_error = None;
    let mut left_press = None;
    let mut confirmation = None::<PendingConfirmation>;
    loop {
        if let Some(project) = dashboard.as_mut() {
            if project_lock
                .as_ref()
                .is_none_or(|lease| lease.workspace() != project.root)
            {
                *project_lock = Some(lock_project(project, text)?);
            }
            if let Some((backend, verbosity)) = runs {
                if let Some(InstallPage::DebugTool(tool)) = install_page {
                    match backend.start_debug(&project.root, &project.manifest, tool) {
                        Ok(process) => {
                            install_page =
                                Some(InstallPage::Run(runs::Page::processes(vec![process], None)));
                            focused_row = Some(1);
                        }
                        Err(error) => {
                            install_page = Some(InstallPage::Detail {
                                title: tool.into(),
                                body: format!("{error:#}"),
                                return_to_ros: false,
                            });
                            focused_row = None;
                        }
                    }
                    scroll = 0;
                    last_process_refresh = None;
                }
                if matches!(install_page, Some(InstallPage::Shell)) {
                    leave_screen(terminal)?;
                    let result = backend.shell(&project.root, &project.manifest);
                    // The main screen may still be at the shell prompt, with raw output enabled.
                    terminal.backend_mut().write_all(b"\r\n")?;
                    execute!(
                        terminal.backend_mut(),
                        EnterAlternateScreen,
                        EnableMouseCapture
                    )?;
                    terminal.hide_cursor()?;
                    terminal.clear()?;
                    install_page = result.err().map(|error| InstallPage::Detail {
                        title: "Shell".into(),
                        body: format!("{error:#}"),
                        return_to_ros: false,
                    });
                    focused_row =
                        first_selectable(Some(project), text, selected, install_page.as_ref());
                    scroll = 0;
                    last_process_refresh = None;
                }
                if let Some(InstallPage::Run(page)) = &mut install_page {
                    if let Some(id) = page.take_task_attach() {
                        leave_screen(terminal)?;
                        disable_raw_mode()?;
                        if let Err(error) = backend.attach_task(&project.root, &id) {
                            eprintln!("{error:#}");
                        }
                        println!("Enter ↩");
                        io::stdin().read_line(&mut String::new())?;
                        enable_raw_mode()?;
                        execute!(
                            terminal.backend_mut(),
                            EnterAlternateScreen,
                            EnableMouseCapture
                        )?;
                        terminal.hide_cursor()?;
                        terminal.clear()?;
                        last_process_refresh = None;
                    }
                    if page.dispatch(*backend, project, *verbosity, text) {
                        install_page = None;
                        focused_row = first_selectable(Some(project), text, selected, None);
                    }
                }
                if last_process_refresh
                    .is_none_or(|last| last.elapsed() >= Duration::from_millis(400))
                {
                    processes = backend.foreground();
                    match backend.background(&project.root) {
                        Ok(background) => {
                            processes.extend(background);
                            process_error = None;
                        }
                        Err(error) => process_error = Some(format!("{error:#}")),
                    }
                    if let Some(id) = backend.pending_authorization().map(str::to_owned) {
                        leave_screen(terminal)?;
                        disable_raw_mode()?;
                        let result = backend.authorize_task(&project.root, &id);
                        enable_raw_mode()?;
                        execute!(
                            terminal.backend_mut(),
                            EnterAlternateScreen,
                            EnableMouseCapture
                        )?;
                        terminal.hide_cursor()?;
                        terminal.clear()?;
                        if let Err(error) = result {
                            process_error = Some(format!("{error:#}"));
                        }
                    }
                    let history = backend.history();
                    let finished: std::collections::HashSet<_> =
                        history.iter().map(|process| &process.id).collect();
                    processes.retain(|process| !finished.contains(&process.id));
                    project.installing = processes
                        .iter()
                        .any(|process| process.task && process.exit.is_none());
                    processes.extend(history);
                    processes.sort_by_key(|process| process.exit.is_some());
                    active_processes = processes
                        .iter()
                        .filter(|process| process.exit.is_none())
                        .count();
                    if let Some(InstallPage::Run(page)) = &mut install_page {
                        page.refresh(&processes, process_error.as_deref(), &mut focused_row, text);
                    }
                    last_process_refresh = Some(Instant::now());
                }
            }
        }
        if last_runtime_refresh.elapsed() >= Duration::from_secs(1) {
            if let Some(browser) = browser.as_mut() {
                browser.reload();
            } else if let Some(dashboard) = dashboard.as_mut() {
                dashboard.refresh_runtime();
            }
            last_runtime_refresh = Instant::now();
        }
        draw_frame(terminal, drawing, |frame| {
            if let Some(browser) = &browser {
                log_view::draw(frame, browser, text);
                return;
            }
            view::draw(
                frame,
                dashboard.as_ref(),
                text,
                selected,
                install_page.as_ref(),
                &mut scroll,
                focused_row,
                runs.is_some().then_some(active_processes),
                confirmation.as_ref().map(|pending| pending.prompt(text)),
            )
        })?;
        if event::poll(Duration::from_millis(250))? {
            match event::read()? {
                Event::Key(key) => {
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && matches!(key.code, KeyCode::Char('c'))
                    {
                        break Ok(());
                    }
                    if let Some(pending) = confirmation.take() {
                        match key.code {
                            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                                execute_pending_confirmation(
                                    pending,
                                    terminal,
                                    active_manifest.as_deref(),
                                    text,
                                    &mut dashboard,
                                    &mut focused_row,
                                    &mut scroll,
                                    selected,
                                    &mut last_runtime_refresh,
                                    build,
                                    action,
                                    task_backend(runs),
                                )?;
                            }
                            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {}
                            _ => confirmation = Some(pending),
                        }
                        continue;
                    }
                    if let Some(log) = browser.as_mut() {
                        match key.code {
                            KeyCode::Esc | KeyCode::Char('q') => browser = None,
                            KeyCode::Tab => log.select(true),
                            KeyCode::BackTab => log.select(false),
                            KeyCode::Char('n') => log.page(true),
                            KeyCode::Char('p') => log.page(false),
                            KeyCode::Char('r') => log.reload(),
                            KeyCode::Down | KeyCode::Char('j') => {
                                log.scroll = (log.scroll + 1).min(log.lines.len().saturating_sub(1))
                            }
                            KeyCode::Up | KeyCode::Char('k') => {
                                log.scroll = log.scroll.saturating_sub(1)
                            }
                            KeyCode::PageDown => {
                                log.scroll =
                                    (log.scroll + 10).min(log.lines.len().saturating_sub(1))
                            }
                            KeyCode::PageUp => log.scroll = log.scroll.saturating_sub(10),
                            KeyCode::Home => log.scroll = 0,
                            KeyCode::End => log.scroll = log.lines.len().saturating_sub(10),
                            KeyCode::Right => log.horizontal = log.horizontal.saturating_add(8),
                            KeyCode::Left => log.horizontal = log.horizontal.saturating_sub(8),
                            _ => {}
                        }
                        continue;
                    }
                    if let Some(InstallPage::Run(page)) = &mut install_page {
                        if page.edit_key(key) {
                            continue;
                        }
                    }
                    if key.code == KeyCode::Char(LOG_KEY) && key.modifiers.is_empty() {
                        browser = Some(open_logs(
                            dashboard.as_ref(),
                            install_page.as_ref(),
                            focused_row,
                            &processes,
                        ));
                        continue;
                    }
                    if key.code == KeyCode::Char(PROCESS_KEY) && runs.is_some() {
                        install_page = Some(InstallPage::Run(runs::Page::processes(
                            processes.clone(),
                            process_error.clone(),
                        )));
                        focused_row = first_selectable(
                            dashboard.as_ref(),
                            text,
                            selected,
                            install_page.as_ref(),
                        );
                        scroll = 0;
                        continue;
                    }
                    let size = terminal.size()?;
                    let page_size = usize::from(size.height.saturating_sub(6).clamp(1, 10));
                    match key.code {
                        KeyCode::Esc | KeyCode::Char('q') if install_page.is_some() => {
                            if back_run(&mut install_page, &mut focused_row, &mut scroll, text) {
                                continue;
                            }
                            install_page = match install_page.take() {
                                Some(InstallPage::Detail {
                                    return_to_ros: true,
                                    ..
                                }) => Some(InstallPage::Ros),
                                _ => None,
                            };
                            scroll = 0;
                            focused_row = first_selectable(
                                dashboard.as_ref(),
                                text,
                                selected,
                                install_page.as_ref(),
                            );
                        }
                        KeyCode::Char('i')
                            if install_page.as_ref().is_some_and(InstallPage::can_install) =>
                        {
                            install(
                                terminal,
                                active_manifest.as_deref(),
                                &mut dashboard,
                                &mut install_page,
                                &mut scroll,
                                text,
                                install_ros,
                                install_components,
                                task_backend(runs),
                                &mut last_runtime_refresh,
                            )?;
                        }
                        KeyCode::Enter => {
                            if let Some(InstallPage::ScannedProjects(projects)) = &install_page {
                                if let Some(path) =
                                    focused_row.and_then(|index| projects.get(index)).cloned()
                                {
                                    open_project(
                                        path,
                                        text,
                                        &mut active_manifest,
                                        &mut dashboard,
                                        project_lock,
                                        &mut install_page,
                                        &mut focused_row,
                                        &mut scroll,
                                    )?;
                                }
                            } else {
                                activate(
                                    &mut install_page,
                                    &mut focused_row,
                                    &mut scroll,
                                    dashboard.as_ref(),
                                    text,
                                    selected,
                                );
                            }
                        }
                        KeyCode::Right if install_page.is_some() => {
                            let rows = pages::rows(
                                dashboard.as_ref(),
                                text,
                                selected,
                                install_page.as_ref(),
                            );
                            if focused_row
                                .and_then(|index| rows.get(index))
                                .is_some_and(|row| row.detail.is_some())
                            {
                                activate(
                                    &mut install_page,
                                    &mut focused_row,
                                    &mut scroll,
                                    dashboard.as_ref(),
                                    text,
                                    selected,
                                );
                            }
                        }
                        KeyCode::Char('q') | KeyCode::Esc => break Ok(()),
                        KeyCode::Left | KeyCode::BackTab if install_page.is_none() => {
                            selected = (selected + 7) % 8;
                            scroll = 0;
                            focused_row =
                                first_selectable(dashboard.as_ref(), text, selected, None);
                        }
                        KeyCode::Right | KeyCode::Tab if install_page.is_none() => {
                            let rows = pages::rows(dashboard.as_ref(), text, selected, None);
                            if focused_row
                                .and_then(|index| rows.get(index))
                                .is_some_and(|row| row.detail.is_some())
                            {
                                activate(
                                    &mut install_page,
                                    &mut focused_row,
                                    &mut scroll,
                                    dashboard.as_ref(),
                                    text,
                                    selected,
                                );
                                continue;
                            }
                            selected = (selected + 1) % 8;
                            scroll = 0;
                            focused_row =
                                first_selectable(dashboard.as_ref(), text, selected, None);
                        }
                        KeyCode::PageDown => scroll = scroll.saturating_add(page_size),
                        KeyCode::PageUp => scroll = scroll.saturating_sub(page_size),
                        KeyCode::Home => scroll = 0,
                        KeyCode::Down | KeyCode::Char('j') => {
                            if !move_focus(
                                &mut focused_row,
                                &mut scroll,
                                dashboard.as_ref(),
                                text,
                                selected,
                                install_page.as_ref(),
                                size,
                                true,
                            ) {
                                scroll = scroll.saturating_add(1);
                            }
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            if !move_focus(
                                &mut focused_row,
                                &mut scroll,
                                dashboard.as_ref(),
                                text,
                                selected,
                                install_page.as_ref(),
                                size,
                                false,
                            ) {
                                scroll = scroll.saturating_sub(1);
                            }
                        }
                        KeyCode::Char('r') => {
                            if let Some(page) = &mut install_page {
                                page.refresh();
                            }
                            dashboard =
                                dashboard::Dashboard::load(active_manifest.as_deref(), text)?;
                            scroll = 0;
                            focused_row = first_selectable(
                                dashboard.as_ref(),
                                text,
                                selected,
                                install_page.as_ref(),
                            );
                            last_runtime_refresh = Instant::now();
                        }
                        KeyCode::Char('n') if selected == 0 && dashboard.is_none() => {
                            request_action(&mut confirmation, Action::CreateProject);
                        }
                        KeyCode::Char('s') if selected == 0 && dashboard.is_none() => {
                            install_page = Some(InstallPage::ScannedProjects(
                                dashboard::Dashboard::scan_projects(&std::env::current_dir()?),
                            ));
                            focused_row = first_selectable(
                                dashboard.as_ref(),
                                text,
                                selected,
                                install_page.as_ref(),
                            );
                            scroll = 0;
                        }
                        KeyCode::Char('b') | KeyCode::Char('B') if selected == 2 => {
                            let rebuild = key.code == KeyCode::Char('B');
                            request_compile(&mut confirmation, rebuild);
                        }
                        KeyCode::Char('a') if selected == 1 => {
                            request_action(&mut confirmation, Action::Apply);
                        }
                        KeyCode::Char('u') if selected == 1 => {
                            request_action(&mut confirmation, Action::Uninstall);
                        }
                        KeyCode::Char('D') if selected == 1 => {
                            request_action(&mut confirmation, Action::DepsInstall);
                        }
                        KeyCode::Char('d') if selected == 1 => {
                            request_action(&mut confirmation, Action::DepsCheck);
                        }
                        KeyCode::Char('c') if selected == 5 => {
                            request_action(&mut confirmation, Action::Check);
                        }
                        KeyCode::Char('v') if selected == 5 => {
                            request_action(&mut confirmation, Action::Verify);
                        }
                        _ => {}
                    }
                }
                Event::Mouse(mouse) => {
                    // Shift-drag remains terminal text selection instead of a TUI action.
                    if mouse.modifiers.contains(KeyModifiers::SHIFT) {
                        left_press = None;
                        continue;
                    }
                    let cell = (mouse.column, mouse.row);
                    match mouse.kind {
                        MouseEventKind::Down(MouseButton::Left) => {
                            left_press = Some(cell);
                            continue;
                        }
                        MouseEventKind::Up(MouseButton::Left)
                            if left_press.take() != Some(cell) =>
                        {
                            continue;
                        }
                        _ => {}
                    }
                    let size = terminal.size()?;
                    if let Some(log) = browser.as_mut() {
                        match mouse.kind {
                            MouseEventKind::ScrollDown => {
                                log.scroll =
                                    (log.scroll + 3).min(log.lines.len().saturating_sub(1));
                            }
                            MouseEventKind::ScrollUp => {
                                log.scroll = log.scroll.saturating_sub(3);
                            }
                            MouseEventKind::Up(MouseButton::Left)
                                if mouse.row == log_view::SOURCE_ROW =>
                            {
                                if let Some(forward) =
                                    log_view::source_direction_at(mouse.column, size.width)
                                {
                                    log.select(forward);
                                }
                            }
                            _ => {}
                        }
                        continue;
                    }
                    if confirmation.is_some() && mouse.row != size.height.saturating_sub(1) {
                        continue;
                    }
                    match mouse.kind {
                        MouseEventKind::Up(MouseButton::Left)
                            if mouse.row == 0
                                && runs.is_some()
                                && view::process_counter_at(
                                    mouse.column,
                                    size.width,
                                    active_processes,
                                    text,
                                ) =>
                        {
                            install_page = Some(InstallPage::Run(runs::Page::processes(
                                processes.clone(),
                                process_error.clone(),
                            )));
                            focused_row = first_selectable(
                                dashboard.as_ref(),
                                text,
                                selected,
                                install_page.as_ref(),
                            );
                            scroll = 0;
                        }
                        MouseEventKind::ScrollDown => scroll = scroll.saturating_add(3),
                        MouseEventKind::ScrollUp => scroll = scroll.saturating_sub(3),
                        MouseEventKind::Up(MouseButton::Left) if mouse.row == 1 => {
                            selected = tab_at(mouse.column, size.width);
                            install_page = None;
                            scroll = 0;
                            focused_row =
                                first_selectable(dashboard.as_ref(), text, selected, None);
                        }
                        MouseEventKind::Up(MouseButton::Left)
                            if mouse.row == size.height.saturating_sub(1) =>
                        {
                            let Some(footer_action) = view::footer_action_at(
                                mouse.column,
                                size.width,
                                install_page.as_ref(),
                                selected,
                                dashboard.is_some(),
                                confirmation.is_some(),
                                text,
                            ) else {
                                continue;
                            };
                            let quit = activate_footer(
                                footer_action,
                                terminal,
                                active_manifest.as_deref(),
                                &mut browser,
                                &processes,
                                &mut dashboard,
                                &mut install_page,
                                &mut focused_row,
                                &mut scroll,
                                &mut selected,
                                text,
                                install_ros,
                                install_components,
                                &mut last_runtime_refresh,
                                build,
                                action,
                                task_backend(runs),
                                &mut confirmation,
                            )?;
                            if quit {
                                break Ok(());
                            }
                        }
                        MouseEventKind::Up(MouseButton::Left) => {
                            let rows = pages::rows(
                                dashboard.as_ref(),
                                text,
                                selected,
                                install_page.as_ref(),
                            );
                            if let Some(index) = table::row_at(
                                view::table_area(ratatui::layout::Rect::new(
                                    0,
                                    0,
                                    size.width,
                                    size.height,
                                )),
                                &rows,
                                text,
                                scroll,
                                mouse.row,
                            ) {
                                if rows.get(index).is_some_and(|row| row.selectable) {
                                    focused_row = Some(index);
                                    if let Some(InstallPage::ScannedProjects(projects)) =
                                        &install_page
                                    {
                                        if let Some(path) = projects.get(index).cloned() {
                                            open_project(
                                                path,
                                                text,
                                                &mut active_manifest,
                                                &mut dashboard,
                                                project_lock,
                                                &mut install_page,
                                                &mut focused_row,
                                                &mut scroll,
                                            )?;
                                        }
                                    } else {
                                        activate(
                                            &mut install_page,
                                            &mut focused_row,
                                            &mut scroll,
                                            dashboard.as_ref(),
                                            text,
                                            selected,
                                        );
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Event::Resize(_, _) => scroll = 0,
                _ => {}
            }
        }
    }
}

fn open_logs(
    dashboard: Option<&dashboard::Dashboard>,
    install_page: Option<&InstallPage>,
    focused_row: Option<usize>,
    processes: &[rospm_core::run::RunStatus],
) -> logs::Browser {
    let preferred = match install_page {
        Some(InstallPage::Run(page)) => page
            .focused_process(focused_row)
            .map(|process| process.id.as_str()),
        _ => None,
    };
    logs::Browser::new(
        dashboard.map(|project| project.root.as_path()),
        processes,
        preferred,
    )
}

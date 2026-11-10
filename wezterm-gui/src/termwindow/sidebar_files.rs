//! Runtime state and event handling for the sidebar's file page.
//!
//! The page's data lives in `crate::sftp`; drawing in
//! `termwindow::render::sidebar_files`.

use crate::sessions::monitor::{cwd_generation, ensure_monitor, pane_cwd};
use crate::sessions::ssh::{remote_ssh_domain, ssh_domain_of_pane};
use crate::sessions::{clamp_scroll, forms, NodeId, SidebarPage};
use crate::sftp::dialogs::{pick_open_files, pick_save_file};
use crate::sftp::model::{
    check_file_name, explain_sftp_error, follow_target, remote_join, remote_parent,
    resolve_path_input, sort_entries, FileEntry,
};
use crate::sftp::transfer::{run_download, run_upload, Direction, TransferJob};
use crate::sftp::{FilesAction, FilesConn, FilesMenu, FilesMenuAction};
use crate::termwindow::form::{Field, FormButton, FormModal, FormState};
use crate::termwindow::sidebar::SidebarFocus;
use crate::termwindow::{TermWindowNotif, UIItem, UIItemType};
use ::window::{Clipboard, MouseEvent, MouseEventKind as WMEK, MousePress, WindowOps};
use mux::domain::DomainId;
use mux::pane::PaneId;
use mux::ssh::RemoteSshDomain;
use std::future::Future;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use wezterm_ssh::{Sftp, SftpChannelError};
use wezterm_term::{KeyCode, KeyModifiers};

/// Rows above the list: the path bar, the buttons, the follow checkbox.
const HEADER_ROWS: usize = 3;

/// The most the transfer line may wrap to.
const MAX_TRANSFER_LINES: usize = 3;

/// How often a running transfer redraws its progress.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

fn describe(err: impl std::fmt::Display) -> String {
    explain_sftp_error(&err.to_string())
}

fn name_errors(state: &FormState) -> Vec<(usize, String)> {
    match check_file_name(state.value("名称")) {
        Some(err) => vec![(0, err.to_string())],
        None => vec![],
    }
}

fn name_form(title: &str, name: &str, ok: &str) -> FormState {
    FormState::new(
        title,
        vec![Field::text("名称", name)],
        vec![
            FormButton::Primary(ok.to_string()),
            FormButton::Cancel("取消".to_string()),
        ],
    )
}

impl crate::TermWindow {
    pub fn files_page_visible(&self) -> bool {
        self.sidebar_is_expanded() && self.sidebar.page == SidebarPage::Files
    }

    /// The sidebar ssh session, connection and pane the page is about: those
    /// of the active pane, when it is one.
    pub fn files_target(&self) -> Option<(NodeId, DomainId, PaneId)> {
        let pane = self.get_active_pane_no_overlay()?;
        let (node, domain) = ssh_domain_of_pane(&*pane)?;
        Some((node, domain, pane.pane_id()))
    }

    /// The page's state for the connection it is showing.
    pub fn files_conn(&self) -> Option<&FilesConn> {
        let (_, domain_id, _) = self.files_target()?;
        self.files.conns.get(&domain_id)
    }

    /// Something the page draws has changed.
    pub(crate) fn files_changed(&mut self) {
        self.files.generation += 1;
        self.invalidate_sidebar();
    }

    /// The notice above the list, broken to fit the panel.
    pub fn files_notice_lines(&self, conn: &FilesConn) -> Vec<String> {
        match &conn.notice {
            Some(notice) => self.files_wrap(notice, self.files_inner_width()),
            None => vec![],
        }
    }

    /// How many entries the list can show at once.
    pub fn files_list_capacity(&self) -> usize {
        let row = self.sidebar_row_height();
        if row <= 0. {
            return 0;
        }
        let rows = (self.sidebar_panel_bounds().height() / row).floor().max(0.) as usize;
        let notice = self
            .files_conn()
            .map(|conn| self.files_notice_lines(conn).len())
            .unwrap_or(0);
        rows.saturating_sub(HEADER_ROWS + notice + self.files_transfer_lines().len())
    }

    /// The transfer line at the bottom of the page, broken to fit beside its
    /// ✕ button.
    pub fn files_transfer_lines(&self) -> Vec<String> {
        let Some(line) = self.files.transfers.status_line() else {
            return vec![];
        };
        let mut lines =
            self.files_wrap(&line, self.files_inner_width() - self.files_cancel_width());
        lines.truncate(MAX_TRANSFER_LINES);
        lines
    }

    /// Make sure there is a page for `domain_id`'s connection, opening one
    /// on first use and after a reconnect. False while the connection is not
    /// up.
    fn files_ensure_conn(&mut self, domain_id: DomainId) -> bool {
        let Some(domain) = remote_ssh_domain(domain_id) else {
            return false;
        };
        let Some(ssh) = domain.downcast_ref::<RemoteSshDomain>() else {
            return false;
        };
        let Some(live) = ssh.authenticated_generation() else {
            return false;
        };
        if self.files.conns.get(&domain_id).map(|c| c.generation) == Some(live) {
            return true;
        }
        let Some((generation, session)) = ssh.authenticated_session() else {
            return false;
        };

        let follow = self
            .files
            .conns
            .get(&domain_id)
            .map(|c| c.follow)
            .unwrap_or(true);
        let sftp = session.sftp();
        self.files.conns.insert(
            domain_id,
            FilesConn {
                generation,
                sftp: sftp.clone(),
                cwd: String::new(),
                home: None,
                entries: vec![],
                loading: true,
                error: None,
                notice: None,
                follow,
                last_followed: None,
                selected: None,
                scroll: 0,
                listing_seq: 0,
            },
        );
        // A fresh page has followed nothing yet.
        self.files.follow_seen = None;

        self.files_spawn(
            domain_id,
            generation,
            async move {
                sftp.canonicalize(".")
                    .await
                    .map(|p| p.to_string())
                    .map_err(describe)
            },
            move |tw, result| tw.files_home_found(domain_id, result),
        );
        self.files_changed();
        true
    }

    /// Run `fut` on the main thread's executor and hand its result to `done`
    /// on this window, unless the connection's page has since been replaced.
    fn files_spawn<T: Send + Sync + 'static>(
        &self,
        domain_id: DomainId,
        generation: u64,
        fut: impl Future<Output = T> + 'static,
        done: impl FnOnce(&mut crate::TermWindow, T) + Send + Sync + 'static,
    ) {
        let Some(window) = self.window.clone() else {
            return;
        };
        promise::spawn::spawn(async move {
            let result = fut.await;
            window.notify(TermWindowNotif::Apply(Box::new(move |tw| {
                let current = tw.files.conns.get(&domain_id).map(|c| c.generation);
                if current == Some(generation) {
                    done(tw, result);
                }
            })));
        })
        .detach();
    }

    /// Run an operation that changes the directory, then list it again; a
    /// failure is shown above the list.
    fn files_run<Fut>(&mut self, domain_id: DomainId, op: impl FnOnce(Sftp) -> Fut)
    where
        Fut: Future<Output = Result<(), String>> + 'static,
    {
        let Some(conn) = self.files.conns.get(&domain_id) else {
            return;
        };
        let generation = conn.generation;
        let fut = op(conn.sftp.clone());
        self.files_spawn(domain_id, generation, fut, move |tw, result| {
            if let Some(conn) = tw.files.conns.get_mut(&domain_id) {
                conn.notice = result.err();
            }
            tw.files_refresh_in(domain_id);
        });
    }

    fn files_home_found(&mut self, domain_id: DomainId, result: Result<String, String>) {
        let Some(conn) = self.files.conns.get_mut(&domain_id) else {
            return;
        };
        // Following may already have taken the page somewhere.
        let go_home = match result {
            Ok(home) => {
                conn.home = Some(home.clone());
                Some(home).filter(|_| conn.cwd.is_empty())
            }
            Err(err) => {
                if conn.cwd.is_empty() {
                    conn.loading = false;
                    conn.error = Some(err);
                }
                None
            }
        };
        match go_home {
            Some(home) => self.files_navigate_in(domain_id, home),
            None => self.files_changed(),
        }
    }

    /// Show `dir` on the page of the active pane's connection.
    pub fn files_navigate(&mut self, dir: String) {
        if let Some((_, domain_id, _)) = self.files_target() {
            self.files_navigate_in(domain_id, dir);
        }
    }

    fn files_navigate_in(&mut self, domain_id: DomainId, dir: String) {
        let Some(conn) = self.files.conns.get_mut(&domain_id) else {
            return;
        };
        if conn.cwd != dir {
            // Another directory's entries under this path would be acted on
            // as if they were here; a big listing can take seconds to come.
            conn.entries.clear();
            conn.error = None;
            conn.selected = None;
            conn.scroll = 0;
            self.files.menu = None;
        }
        conn.cwd = dir.clone();
        conn.loading = true;
        conn.listing_seq += 1;
        let seq = conn.listing_seq;
        let generation = conn.generation;
        let sftp = conn.sftp.clone();
        self.files_spawn(
            domain_id,
            generation,
            async move { crate::sftp::list_dir(&sftp, &dir).await },
            move |tw, result| tw.files_listing_done(domain_id, seq, result),
        );
        self.files_changed();
    }

    fn files_listing_done(
        &mut self,
        domain_id: DomainId,
        seq: u64,
        result: Result<Vec<FileEntry>, String>,
    ) {
        let capacity = self.files_list_capacity();
        let Some(conn) = self.files.conns.get_mut(&domain_id) else {
            return;
        };
        if conn.listing_seq != seq {
            // A later request has been made; its answer is the one to show.
            return;
        }
        conn.loading = false;
        match result {
            Ok(mut entries) => {
                sort_entries(&mut entries);
                conn.entries = entries;
                conn.error = None;
            }
            Err(err) => {
                conn.entries.clear();
                conn.error = Some(err);
            }
        }
        if conn.selected.map_or(false, |i| i >= conn.entries.len()) {
            conn.selected = None;
        }
        conn.scroll = clamp_scroll(conn.entries.len(), conn.scroll, capacity);
        // The menu's row index refers to the old listing.
        self.files.menu = None;
        self.files_changed();
    }

    pub fn files_refresh(&mut self) {
        if let Some((_, domain_id, _)) = self.files_target() {
            self.files_refresh_in(domain_id);
        }
    }

    fn files_refresh_in(&mut self, domain_id: DomainId) {
        let cwd = match self.files.conns.get(&domain_id) {
            Some(conn) if !conn.cwd.is_empty() => conn.cwd.clone(),
            _ => return,
        };
        self.files_navigate_in(domain_id, cwd);
    }

    /// Called at the start of each paint: open the page's connection when
    /// it is first shown, and follow the terminal's directory.
    ///
    /// Following is judged again only when some pane's directory has changed
    /// or a different pane has become active, so browsing elsewhere by hand
    /// sticks until the terminal moves.
    pub fn files_tick_follow(&mut self) {
        if !self.files_page_visible() {
            return;
        }
        let target = self.files_target();
        let Some((_, domain_id, pane_id)) = target else {
            let seen = (cwd_generation(), None);
            if self.files.follow_seen != Some(seen) {
                self.files.follow_seen = Some(seen);
                self.files_changed();
            }
            return;
        };

        // The directories come from the status channel; it has to be running
        // even when the status bar is turned off.
        ensure_monitor(domain_id);
        if !self.files_ensure_conn(domain_id) {
            return;
        }

        let seen = (cwd_generation(), Some((domain_id, pane_id)));
        if self.files.follow_seen == Some(seen) {
            return;
        }
        let other_pane = self.files.follow_seen.map(|(_, t)| t) != Some(seen.1);
        self.files.follow_seen = Some(seen);

        let reported = pane_cwd(domain_id, pane_id);
        let jump = self.files.conns.get(&domain_id).and_then(|conn| {
            follow_target(
                conn.follow,
                reported.as_deref(),
                conn.last_followed.as_deref(),
            )
        });
        match jump {
            Some(dir) => {
                if let Some(conn) = self.files.conns.get_mut(&domain_id) {
                    conn.last_followed = Some(dir.clone());
                }
                self.files_navigate_in(domain_id, dir);
            }
            None if other_pane => self.files_changed(),
            None => {}
        }
    }

    pub fn files_action(&mut self, action: FilesAction) {
        if action == FilesAction::CancelTransfer {
            // About the queue, whichever server the page shows now.
            let queue = &mut self.files.transfers;
            if queue.is_busy() {
                queue.cancel_active();
            } else {
                queue.last_result = None;
                queue.note = None;
            }
            self.files_changed();
            return;
        }
        let Some((_, domain_id, _)) = self.files_target() else {
            return;
        };
        let Some(conn) = self.files.conns.get(&domain_id) else {
            return;
        };
        let cwd = conn.cwd.clone();
        let home = conn.home.clone();
        match action {
            FilesAction::Up => {
                if !cwd.is_empty() {
                    self.files_navigate_in(domain_id, remote_parent(&cwd));
                }
            }
            FilesAction::Refresh => self.files_refresh_in(domain_id),
            FilesAction::Home => {
                if let Some(home) = home {
                    self.files_navigate_in(domain_id, home);
                }
            }
            FilesAction::NewFolder => self.files_new_folder(domain_id),
            FilesAction::Upload => self.files_upload(domain_id),
            // Handled before looking at the page's connection.
            FilesAction::CancelTransfer => {}
        }
    }

    pub fn files_menu_action(&mut self, action: FilesMenuAction) {
        let Some(menu) = self.files.menu.take() else {
            return;
        };
        self.files_changed();
        let Some((_, domain_id, _)) = self.files_target() else {
            return;
        };
        match (action, menu.target) {
            (FilesMenuAction::Download, Some(i)) => {
                if let Some((path, entry)) = self.files_entry(domain_id, i) {
                    self.files_download(domain_id, path, entry.size);
                }
            }
            (FilesMenuAction::Rename, Some(i)) => self.files_rename(domain_id, i),
            (FilesMenuAction::Delete, Some(i)) => self.files_delete(domain_id, i),
            (FilesMenuAction::CopyPath, Some(i)) => {
                if let Some((path, _)) = self.files_entry(domain_id, i) {
                    if let Some(window) = self.window.as_ref() {
                        window.set_clipboard(Clipboard::Clipboard, path);
                    }
                }
            }
            (FilesMenuAction::NewFolder, _) => self.files_new_folder(domain_id),
            (FilesMenuAction::Refresh, _) => self.files_refresh_in(domain_id),
            _ => {}
        }
    }

    /// The full remote path of row `index`, and the row.
    fn files_entry(&self, domain_id: DomainId, index: usize) -> Option<(String, FileEntry)> {
        let conn = self.files.conns.get(&domain_id)?;
        let entry = conn.entries.get(index)?.clone();
        Some((remote_join(&conn.cwd, &entry.name), entry))
    }

    /// Double-click: go into a directory, fetch a file.
    fn files_open(&mut self, domain_id: DomainId, index: usize) {
        let Some((path, entry)) = self.files_entry(domain_id, index) else {
            return;
        };
        if entry.is_dir {
            self.files_navigate_in(domain_id, path);
        } else if entry.is_link {
            // Listings describe the link itself; ask what it points at.
            let Some(conn) = self.files.conns.get(&domain_id) else {
                return;
            };
            let sftp = conn.sftp.clone();
            let generation = conn.generation;
            let target = path.clone();
            self.files_spawn(
                domain_id,
                generation,
                async move { sftp.metadata(target.as_str()).await.map_err(describe) },
                move |tw, result| match result {
                    Ok(meta) if meta.ty.is_dir() => tw.files_navigate_in(domain_id, path),
                    Ok(meta) => tw.files_download(domain_id, path, meta.size),
                    Err(err) => {
                        if let Some(conn) = tw.files.conns.get_mut(&domain_id) {
                            conn.notice = Some(err);
                        }
                        tw.files_changed();
                    }
                },
            );
        } else {
            self.files_download(domain_id, path, entry.size);
        }
    }

    /// The 上传 button: pick files and send them to the directory shown.
    pub fn files_upload(&mut self, domain_id: DomainId) {
        let Some(window) = self.window.clone() else {
            return;
        };
        promise::spawn::spawn(async move {
            let paths = pick_open_files(true).await;
            if !paths.is_empty() {
                window.notify(TermWindowNotif::Apply(Box::new(move |tw| {
                    tw.files_upload_paths(domain_id, paths)
                })));
            }
        })
        .detach();
    }

    /// Files dropped on the window. Onto the file page they are uploaded to
    /// the directory it shows; returns false for anywhere else, where they
    /// keep their usual meaning.
    pub fn files_drop(&mut self, paths: &[PathBuf]) -> bool {
        if !self.files_page_visible() {
            return false;
        }
        let Some((x, y)) = self.files_drop_point() else {
            return false;
        };
        if !self.sidebar_bounds().contains(euclid::point2(x, y)) {
            return false;
        }
        let Some((_, domain_id, _)) = self.files_target() else {
            return false;
        };
        self.files_upload_paths(domain_id, paths.to_vec());
        true
    }

    /// Where the pointer is, in window pixels. A drop arrives without a
    /// position, and no mouse events come while something is being dragged
    /// in from another program, so on Windows ask the system.
    fn files_drop_point(&self) -> Option<(f32, f32)> {
        #[cfg(windows)]
        {
            use ::window::raw_window_handle::{HasWindowHandle, RawWindowHandle};
            use winapi::shared::windef::{HWND, POINT};
            use winapi::um::winuser::{GetCursorPos, ScreenToClient};
            let window = self.window.as_ref()?;
            let handle = window.window_handle().ok()?;
            let RawWindowHandle::Win32(h) = handle.as_raw() else {
                return None;
            };
            let mut pt = POINT { x: 0, y: 0 };
            unsafe {
                if GetCursorPos(&mut pt) == 0 || ScreenToClient(h.hwnd.get() as HWND, &mut pt) == 0
                {
                    return None;
                }
            }
            Some((pt.x as f32, pt.y as f32))
        }
        #[cfg(not(windows))]
        {
            self.current_mouse_event
                .as_ref()
                .map(|event| (event.coords.x as f32, event.coords.y as f32))
        }
    }

    /// Queue local files for upload to the directory the page shows for
    /// `domain_id`, asking first about any that would replace a remote file.
    pub fn files_upload_paths(&mut self, domain_id: DomainId, paths: Vec<PathBuf>) {
        let Some(conn) = self.files.conns.get(&domain_id) else {
            return;
        };
        if conn.cwd.is_empty() {
            return;
        }
        let cwd = conn.cwd.clone();
        let sftp = conn.sftp.clone();
        let generation = conn.generation;

        let mut files = vec![];
        for path in paths {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            match std::fs::metadata(&path) {
                Ok(meta) if meta.is_dir() => {
                    self.files
                        .transfers
                        .note(format!("暂不支持上传文件夹：{name}"));
                }
                Ok(meta) => files.push((path, remote_join(&cwd, &name), meta.len())),
                Err(err) => self.files.transfers.note(format!("无法读取 {name}：{err}")),
            }
        }
        self.files_changed();
        if files.is_empty() {
            return;
        }

        self.files_spawn(
            domain_id,
            generation,
            async move {
                let mut checked = vec![];
                for (local, remote, size) in files {
                    let exists = sftp.metadata(remote.as_str()).await.is_ok();
                    checked.push((local, remote, size, exists));
                }
                checked
            },
            move |tw, checked| {
                for (local, remote, size, exists) in checked {
                    let job = tw.files_new_job(
                        domain_id,
                        Direction::Upload { local, remote },
                        Some(size),
                    );
                    if exists {
                        tw.files.overwrite_pending.push_back(job);
                    } else {
                        tw.files.transfers.push(job);
                    }
                }
                tw.files_pump_transfers();
                tw.files_ask_overwrite();
            },
        );
    }

    /// Save a remote file somewhere the user picks.
    pub fn files_download(&mut self, domain_id: DomainId, path: String, size: Option<u64>) {
        let Some(window) = self.window.clone() else {
            return;
        };
        let name = path.rsplit('/').next().unwrap_or("download").to_string();
        promise::spawn::spawn(async move {
            let Some(local) = pick_save_file(&name).await else {
                return;
            };
            window.notify(TermWindowNotif::Apply(Box::new(move |tw| {
                let job = tw.files_new_job(
                    domain_id,
                    Direction::Download {
                        remote: path,
                        local,
                    },
                    size,
                );
                tw.files.transfers.push(job);
                tw.files_pump_transfers();
                tw.files_changed();
            })));
        })
        .detach();
    }

    fn files_new_job(
        &mut self,
        domain_id: DomainId,
        dir: Direction,
        size: Option<u64>,
    ) -> TransferJob {
        self.files.next_transfer_id += 1;
        TransferJob {
            id: self.files.next_transfer_id,
            domain: domain_id,
            dir,
            size,
        }
    }

    /// Ask about the next upload that would replace a remote file. One
    /// question at a time: answering it asks the next.
    fn files_ask_overwrite(&mut self) {
        if self.get_modal().is_some() {
            return;
        }
        let Some(job) = self.files.overwrite_pending.pop_front() else {
            return;
        };
        let message = format!("远端已有同名文件「{}」，覆盖吗？", job.name());
        let form = FormModal::new(forms::confirm_form("覆盖文件", &message, "覆盖"))
            .on_submit(move |_, tw| {
                tw.files.transfers.push(job.clone());
                tw.files_pump_transfers();
                tw.files_changed();
                tw.files_ask_overwrite();
            })
            .on_cancel(|tw| tw.files_ask_overwrite())
            // It appears a round trip after the drop, maybe while typing.
            .ignore_early_enter();
        self.open_form(form);
    }

    /// Start the next queued transfer, if none is running.
    fn files_pump_transfers(&mut self) {
        while let Some((job, cancel)) = self.files.transfers.start_next() {
            let sftp = self.files.conns.get(&job.domain).map(|c| c.sftp.clone());
            let (Some(sftp), Some(window)) = (sftp, self.window.clone()) else {
                self.files
                    .transfers
                    .finish(job.id, Err("连接已断开".to_string()));
                continue;
            };
            let id = job.id;
            let domain_id = job.domain;
            promise::spawn::spawn(async move {
                let progress_window = window.clone();
                let mut last = Instant::now();
                let report = move |done: u64| {
                    if last.elapsed() >= PROGRESS_INTERVAL {
                        last = Instant::now();
                        progress_window.notify(TermWindowNotif::Apply(Box::new(move |tw| {
                            tw.files.transfers.progress(id, done);
                            tw.files_changed();
                        })));
                    }
                };
                let upload = matches!(job.dir, Direction::Upload { .. });
                let result = match job.dir {
                    Direction::Upload { local, remote } => {
                        run_upload(sftp, local, remote, cancel, report).await
                    }
                    Direction::Download { remote, local } => {
                        run_download(sftp, remote, local, cancel, report).await
                    }
                };
                window.notify(TermWindowNotif::Apply(Box::new(move |tw| {
                    tw.files_transfer_done(id, domain_id, upload, result)
                })));
            })
            .detach();
            break;
        }
        self.files_changed();
    }

    fn files_transfer_done(
        &mut self,
        id: u64,
        domain_id: DomainId,
        upload: bool,
        result: Result<(), String>,
    ) {
        if let Err(err) = &result {
            log::warn!("file transfer {id} ended: {err}");
        }
        self.files.transfers.finish(id, result);
        if upload {
            // Show the new file, or that a failed one is gone again.
            self.files_refresh_in(domain_id);
        }
        self.files_changed();
        self.files_pump_transfers();
        self.files_ask_overwrite();
    }

    fn files_new_folder(&mut self, domain_id: DomainId) {
        let Some(cwd) = self
            .files
            .conns
            .get(&domain_id)
            .map(|c| c.cwd.clone())
            .filter(|c| !c.is_empty())
        else {
            return;
        };
        let form = FormModal::new(name_form("新建文件夹", "", "创建"))
            .validate(name_errors)
            .on_submit(move |state, tw| {
                let path = remote_join(&cwd, state.value("名称").trim());
                tw.files_run(domain_id, move |sftp| async move {
                    sftp.create_dir(path.as_str(), 0o755)
                        .await
                        .map_err(|err| format!("新建文件夹失败：{}", describe(err)))
                });
            });
        self.open_form(form);
    }

    fn files_rename(&mut self, domain_id: DomainId, index: usize) {
        let Some((old, entry)) = self.files_entry(domain_id, index) else {
            return;
        };
        let parent = remote_parent(&old);
        let form = FormModal::new(name_form("重命名", &entry.name, "确定"))
            .validate(name_errors)
            .on_submit(move |state, tw| {
                let name = state.value("名称").trim().to_string();
                if name == entry.name {
                    return;
                }
                let old = old.clone();
                let new = remote_join(&parent, &name);
                tw.files_run(domain_id, move |sftp| async move {
                    crate::sftp::rename_file(&sftp, &old, &new).await
                });
            });
        self.open_form(form);
    }

    /// Delete a file, or a directory only if it is empty.
    fn files_delete(&mut self, domain_id: DomainId, index: usize) {
        let Some((path, entry)) = self.files_entry(domain_id, index) else {
            return;
        };
        let what = if entry.is_dir { "文件夹" } else { "文件" };
        let message = format!("删除{what} {path}？");
        let form = FormModal::new(forms::confirm_form("删除", &message, "删除")).on_submit(
            move |_, tw| {
                let path = path.clone();
                let is_dir = entry.is_dir;
                tw.files_run(domain_id, move |sftp| async move {
                    fn failed(err: SftpChannelError) -> String {
                        format!("删除失败：{}", describe(err))
                    }
                    if is_dir {
                        let empty = crate::sftp::dir_is_empty(&sftp, &path)
                            .await
                            .map_err(|err| format!("删除失败：{err}"))?;
                        if !empty {
                            return Err(format!("目录不为空：{path}"));
                        }
                        sftp.remove_dir(path.as_str()).await.map_err(failed)
                    } else {
                        sftp.remove_file(path.as_str()).await.map_err(failed)
                    }
                });
            },
        );
        self.open_form(form);
    }

    fn files_toggle_follow(&mut self, domain_id: DomainId) {
        if let Some(conn) = self.files.conns.get_mut(&domain_id) {
            conn.follow = !conn.follow;
            if conn.follow {
                // Turning it on means "take me to the terminal", even if that
                // is where it last went.
                conn.last_followed = None;
            }
        }
        self.files.follow_seen = None;
        self.files_changed();
    }

    fn files_scroll(&mut self, lines: isize) {
        let capacity = self.files_list_capacity();
        let Some((_, domain_id, _)) = self.files_target() else {
            return;
        };
        let Some(conn) = self.files.conns.get_mut(&domain_id) else {
            return;
        };
        let offset = (conn.scroll as isize + lines).max(0) as usize;
        conn.scroll = clamp_scroll(conn.entries.len(), offset, capacity);
        self.files_changed();
    }

    pub fn close_files_menu(&mut self) {
        if self.files.menu.take().is_some() {
            self.files_changed();
        }
    }

    fn open_files_menu(&mut self, domain_id: DomainId, target: Option<usize>, x: f32, y: f32) {
        let items = match target.and_then(|i| self.files_entry(domain_id, i)) {
            Some((_, entry)) => {
                let mut items = vec![];
                if !entry.is_dir {
                    items.push(FilesMenuAction::Download);
                }
                items.extend([
                    FilesMenuAction::Rename,
                    FilesMenuAction::Delete,
                    FilesMenuAction::CopyPath,
                    FilesMenuAction::NewFolder,
                    FilesMenuAction::Refresh,
                ]);
                items
            }
            None => vec![FilesMenuAction::NewFolder, FilesMenuAction::Refresh],
        };
        if let Some(conn) = self.files.conns.get_mut(&domain_id) {
            if target.is_some() {
                conn.selected = target;
            }
        }
        self.files.menu = Some(FilesMenu {
            target,
            x,
            y,
            items,
        });
        self.files_changed();
    }

    // --- path bar -------------------------------------------------------

    fn files_begin_path_edit(&mut self) {
        let Some(cwd) = self.files_conn().map(|c| c.cwd.clone()) else {
            return;
        };
        self.files.path_edit = Some(cwd);
        self.files.path_fresh = true;
        self.sidebar.focus = Some(SidebarFocus::FilesPath);
        self.sidebar.composing = None;
        self.files_changed();
    }

    /// Keys while the path bar is being edited.
    pub fn files_path_key_down(&mut self, key: &KeyCode, mods: KeyModifiers) -> bool {
        match (key, mods) {
            (KeyCode::Escape, _) => self.release_sidebar_focus(),
            (KeyCode::Enter, _) => {
                let text = self.files.path_edit.clone().unwrap_or_default();
                self.release_sidebar_focus();
                if let Some((_, domain_id, _)) = self.files_target() {
                    let dir = self.files.conns.get(&domain_id).and_then(|conn| {
                        resolve_path_input(&text, &conn.cwd, conn.home.as_deref())
                    });
                    if let Some(dir) = dir {
                        self.files_navigate_in(domain_id, dir);
                    }
                }
            }
            (KeyCode::Backspace, _) => {
                self.files.path_fresh = false;
                if let Some(text) = self.files.path_edit.as_mut() {
                    text.pop();
                }
            }
            (KeyCode::Char('u'), KeyModifiers::CTRL) => {
                self.files.path_fresh = false;
                if let Some(text) = self.files.path_edit.as_mut() {
                    text.clear();
                }
            }
            (KeyCode::Char(c), KeyModifiers::NONE) | (KeyCode::Char(c), KeyModifiers::SHIFT) => {
                self.files_path_type(&c.to_string());
            }
            _ => return false,
        }
        self.files_changed();
        true
    }

    /// Text typed or committed by the IME into the path bar.
    pub fn files_path_type(&mut self, text: &str) {
        let fresh = std::mem::take(&mut self.files.path_fresh);
        if let Some(path) = self.files.path_edit.as_mut() {
            if fresh {
                path.clear();
            }
            path.push_str(text);
        }
    }

    // --- mouse ----------------------------------------------------------

    pub fn mouse_event_files(&mut self, item: UIItem, event: MouseEvent, _context: &dyn WindowOps) {
        if let WMEK::VertWheel(amount) = event.kind {
            self.files_scroll(-(amount as isize));
            return;
        }

        let left = matches!(event.kind, WMEK::Press(MousePress::Left));
        let right = matches!(event.kind, WMEK::Press(MousePress::Right));
        if !left && !right {
            return;
        }
        if item.item_type != UIItemType::FilesPath
            && self.sidebar.focus == Some(SidebarFocus::FilesPath)
        {
            self.release_sidebar_focus();
            self.files_changed();
        }

        let Some((_, domain_id, _)) = self.files_target() else {
            return;
        };
        let (x, y) = (event.coords.x as f32, event.coords.y as f32);
        match item.item_type {
            UIItemType::FilesRow(index) => {
                if right {
                    self.open_files_menu(domain_id, Some(index), x, y);
                    return;
                }
                let streak = self
                    .last_mouse_click
                    .as_ref()
                    .map(|click| click.streak)
                    .unwrap_or(1);
                match streak {
                    1 => {
                        if let Some(conn) = self.files.conns.get_mut(&domain_id) {
                            conn.selected = Some(index);
                        }
                        self.files_changed();
                    }
                    2 => self.files_open(domain_id, index),
                    _ => {}
                }
            }
            UIItemType::FilesBackground => {
                if right {
                    self.open_files_menu(domain_id, None, x, y);
                }
            }
            UIItemType::FilesToolbar(action) if left => self.files_action(action),
            UIItemType::FilesPath if left => self.files_begin_path_edit(),
            UIItemType::FilesFollow if left => self.files_toggle_follow(domain_id),
            UIItemType::FilesMenuItem(action) if left => self.files_menu_action(action),
            _ => {}
        }
    }
}

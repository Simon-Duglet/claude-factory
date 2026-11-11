mod attach;
mod history;
mod live;
mod prompt;
mod runner;
mod scene;
mod setup;
mod tasks;
mod team;

use bevy::camera::visibility::RenderLayers;
use bevy::camera::{CameraOutputMode, Viewport};
use bevy::prelude::*;
use bevy::render::render_resource::BlendState;
use bevy::window::{PrimaryWindow, WindowResolution};
use bevy_egui::egui::{self, Color32, Key, KeyboardShortcut, LayerId, Modifiers, UiBuilder};
use bevy_egui::{EguiContexts, EguiGlobalSettings, EguiPlugin, EguiPrimaryContextPass, PrimaryEguiContext};
use runner::{Agent, Entry, Status};
use scene::{NodeSpec, NodeState, SceneCamera, SceneInput, SceneOutput};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use team::{Member, PastSession, Role, Store, Team};

/// Agents the app is running, keyed by (team name, member name).
type Running = HashMap<(String, String), Agent>;

#[derive(PartialEq)]
enum View {
    Design,
    Chat,
}

const MODELS: [&str; 4] = ["", "opus", "sonnet", "haiku"];
const IDLE: Color32 = Color32::from_rgb(242, 173, 56);
const FAILED: Color32 = Color32::from_rgb(235, 82, 82);

fn main() {
    if std::env::args().nth(1).as_deref() == Some("session-start") {
        session_start();
        return;
    }
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Claude Factory".into(),
                // Wayland app_id / X11 WM_CLASS; matches StartupWMClass in claude-factory.desktop.
                name: Some("claude-factory".into()),
                resolution: WindowResolution::new(1440, 900),
                ..default()
            }),
            ..default()
        }))
        .add_plugins(EguiPlugin::default())
        .add_plugins(scene::ScenePlugin)
        .insert_resource(Factory::new())
        .add_systems(Startup, setup_cameras)
        .add_systems(EguiPrimaryContextPass, ui_system)
        .add_systems(Update, set_window_icon)
        .run();
}

/// Sets res/logo-256.png as the window icon once winit has created the window. Wayland ignores it and
/// takes the icon from claude-factory.desktop instead.
fn set_window_icon(window: Query<Entity, With<PrimaryWindow>>, mut done: Local<bool>, _main_thread: bevy::ecs::system::NonSendMarker) {
    let Ok(entity) = window.single() else { return };
    if *done {
        return;
    }
    bevy::winit::WINIT_WINDOWS.with_borrow(|windows| {
        let Some(window) = windows.get_window(entity) else { return };
        let image = image::load_from_memory(include_bytes!("../res/logo-256.png")).expect("res/logo-256.png is a valid PNG");
        // 128 px: 256x256 is just over X11's 65535-word request limit and the property is silently dropped.
        let image = image.resize(128, 128, image::imageops::FilterType::Lanczos3).into_rgba8();
        let (w, h) = image.dimensions();
        window.set_window_icon(winit::window::Icon::from_rgba(image.into_raw(), w, h).ok());
        *done = true;
    });
}

fn setup_cameras(mut commands: Commands, mut egui_settings: ResMut<EguiGlobalSettings>) {
    // The scene camera only draws in the area the panels leave free; egui gets its own camera on top.
    egui_settings.auto_create_primary_context = false;
    commands.spawn((Camera2d, SceneCamera));
    commands.spawn((
        PrimaryEguiContext,
        Camera2d,
        RenderLayers::none(),
        Camera {
            order: 1,
            output_mode: CameraOutputMode::Write {
                blend_state: Some(BlendState::ALPHA_BLENDING),
                clear_color: ClearColorConfig::None,
            },
            clear_color: ClearColorConfig::Custom(Color::NONE),
            ..default()
        },
    ));
}

#[derive(Resource)]
struct Factory {
    store: Store,
    saved: String,
    selected: usize,
    status: String,
    sessions: Vec<live::Session>,
    /// The selected team's shared task list, refreshed with the live sessions.
    tasks: Vec<tasks::Task>,
    last_poll: Instant,
    installed: bool,
    view: View,
    running: Running,
    /// The random suffix of each running team's session names, by team name; the UI strips it back off.
    tags: HashMap<String, String>,
    chat_with: String,
    draft: String,
    /// Files pasted into the chat input, sent with the next message.
    attachments: Vec<std::path::PathBuf>,
    /// The agent copied from the scene's context menu, for Paste.
    copied: Option<Member>,
    /// The node the scene's context menu was opened on; None on empty canvas.
    menu_on: Option<String>,
    history: HistoryView,
    /// Notifications agents raised, as (team, message), shown until dismissed.
    notifications: Vec<(String, String)>,
    /// The user's picks on pending questions, by request id: per question, the options ticked and the "Other" text.
    answers: HashMap<String, Vec<(Vec<bool>, String)>>,
    /// When each enabled routine fires next, by (team, routine name, schedule) so an edit reschedules it.
    routine_next: HashMap<(String, String, team::Schedule), i64>,
}

/// The chat panel's History mode: an agent's past conversations, and the one being read.
#[derive(Default)]
struct HistoryView {
    open: bool,
    /// The (team, agent) `list` was built for; None rebuilds it.
    for_key: Option<(String, String)>,
    list: Vec<history::Summary>,
    viewing: Option<(history::Summary, Vec<Entry>)>,
}

impl Factory {
    fn new() -> Self {
        let store = Store::load();
        let saved = serde_json::to_string(&store).unwrap_or_default();
        runner::reap_leftovers();
        let status = first_run_setup();
        Self {
            store,
            saved,
            selected: 0,
            status,
            sessions: live::read_sessions(),
            tasks: Vec::new(),
            last_poll: Instant::now(),
            installed: setup::is_installed(),
            view: View::Design,
            running: Running::new(),
            tags: HashMap::new(),
            chat_with: String::new(),
            draft: String::new(),
            attachments: Vec::new(),
            copied: None,
            menu_on: None,
            history: HistoryView::default(),
            notifications: Vec::new(),
            answers: HashMap::new(),
            routine_next: HashMap::new(),
        }
    }

    // Autosave: the store is tiny, so comparing its JSON every frame is cheaper than tracking edits.
    fn autosave(&mut self) {
        let Ok(now) = serde_json::to_string(&self.store) else { return };
        if now != self.saved {
            self.status = match self.store.save() {
                Ok(()) => "Saved".into(),
                Err(e) => format!("Save failed: {e}"),
            };
            self.saved = now;
        }
    }

    fn poll_live(&mut self) {
        if self.last_poll.elapsed() >= Duration::from_secs(1) {
            self.sessions = live::read_sessions();
            self.tasks = self.store.teams.get(self.selected).map(|t| tasks::read(&t.task_list_id())).unwrap_or_default();
            // Agents set themselves as owner under their tagged session name.
            let tag = self.store.teams.get(self.selected).and_then(|t| self.tags.get(&t.name));
            for task in &mut self.tasks {
                if let (Some(owner), Some(tag)) = (&mut task.owner, tag) {
                    *owner = untag(owner, tag);
                }
            }
            self.record_sessions();
            self.run_routines();
            self.last_poll = Instant::now();
        }
    }

    /// Sends each due routine's prompt to its team's lead. A run that finds the lead stopped is skipped, not queued,
    /// and runs missed while the app was closed are not caught up.
    fn run_routines(&mut self) {
        let now = history::now();
        let mut live = HashMap::new();
        for team in &mut self.store.teams {
            let lead = (team.name.clone(), team.lead_name());
            for r in team.routines.iter_mut().filter(|r| r.enabled) {
                let key = (team.name.clone(), r.name.clone(), r.schedule.clone());
                let next = *self.routine_next.entry(key.clone()).or_insert_with(|| {
                    let n = r.schedule.next_after(r.last_run);
                    if n > now { n } else { r.schedule.next_after(now) }
                });
                if now < next {
                    live.insert(key, next);
                    continue;
                }
                live.insert(key, r.schedule.next_after(now));
                if let Some(agent) = self.running.get_mut(&lead)
                    && agent.status() != Status::Exited
                {
                    agent.send(&format!("[Routine: {}]\n{}", r.name, r.prompt.trim()), &[]);
                    r.last_run = now;
                }
            }
        }
        // Drops disabled, renamed and rescheduled routines, so they start fresh when they come back.
        self.routine_next = live;
    }

    /// Saves each running agent's session to the store's history, with its first message once it has one.
    fn record_sessions(&mut self) {
        for ((team, name), agent) in &self.running {
            let Some(id) = agent.session_id() else { continue };
            let i = self.store.history.iter().position(|h| h.id == id).unwrap_or_else(|| {
                let started_at = history::now();
                self.store.history.push(PastSession { team: team.clone(), agent: name.clone(), id, started_at, preview: String::new() });
                self.store.history.len() - 1
            });
            let past = &mut self.store.history[i];
            if past.preview.is_empty()
                && let Some((from, text)) = agent.first_message()
            {
                let first = match from {
                    Some(from) => format!("From {}: {text}", history::untag(&from)),
                    None => text,
                };
                past.preview = history::preview(&first);
            }
        }
    }
}

/// Refreshes the bundled mod on disk and, the first time, sets up Claude. Returns the status line text.
fn first_run_setup() -> String {
    if let Err(e) = setup::write_mod() {
        return format!("Could not write the mod to {}: {e}", setup::mod_dir());
    }
    if setup::is_installed() {
        return String::new();
    }
    match setup::install() {
        Ok(()) => "Claude set up. Start a new Claude session to pick it up.".into(),
        Err(e) => format!("Setup failed: {e}"),
    }
}

fn ui_system(
    mut contexts: EguiContexts,
    mut factory: ResMut<Factory>,
    mut scene: ResMut<SceneInput>,
    mut output: ResMut<SceneOutput>,
    mut camera: Single<&mut Camera, With<SceneCamera>>,
    window: Single<&Window, With<PrimaryWindow>>,
) -> Result {
    let ctx = contexts.ctx_mut()?.clone();
    let f = &mut *factory;
    f.poll_live();
    let delivered = route_messages(&f.running, &f.tags);
    for ((team, _), agent) in &f.running {
        for text in agent.take_notifications() {
            // Best effort: no notify-send (or no notification daemon) still leaves the in-app toast.
            let _ = std::process::Command::new("notify-send").args(["--app-name=Claude Factory", &format!("Claude Factory: {team}"), &text]).spawn();
            f.notifications.push((team.clone(), text));
        }
    }
    if let Some(target) = output.right_clicked.take() {
        f.menu_on = target;
    }
    // A left click on a context menu item also lands on the node under it.
    if let Some(key) = output.clicked.take()
        && !egui::Popup::is_any_open(&ctx)
    {
        f.chat_with = key;
        f.view = View::Chat;
    }

    let mut root = egui::Ui::new(ctx.clone(), "root".into(), UiBuilder::new().layer_id(LayerId::background()).max_rect(ctx.viewport_rect()));

    let top = egui::Panel::top("top")
        .show(&mut root, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Claude Factory");
                ui.separator();
                if f.installed {
                    ui.colored_label(Color32::LIGHT_GREEN, "Claude is set up: agent teams on, team-factory mod loaded");
                } else if ui
                    .button("Retry Claude setup")
                    .on_hover_text(format!(
                        "Adds to ~/.claude/settings.json env:\n  {}=1\n  {} += {}\nA backup is written next to it.",
                        setup::TEAMS_VAR,
                        setup::PLUGIN_DIRS_VAR,
                        setup::mod_dir()
                    ))
                    .clicked()
                {
                    f.status = first_run_setup();
                    f.installed = setup::is_installed();
                }
                ui.separator();
                ui.checkbox(&mut f.store.desktop_preview, "Desktop previews")
                    .on_hover_text("Live thumbnails of the agents' nested desktops. Off: no screenshots are taken.");
                runner::DESKTOP_PREVIEW.store(f.store.desktop_preview, std::sync::atomic::Ordering::Relaxed);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.label(&f.status));
            });
        })
        .response
        .rect
        .height();

    let left = egui::Panel::left("teams")
        .default_size(190.0)
        .show(&mut root, |ui| {
            ui.heading("Teams");
            for (i, t) in f.store.teams.iter().enumerate() {
                let live = f.sessions.iter().filter(|s| uses_team(&f.store, s, &t.name)).count();
                let mut label = t.name.clone();
                if !t.enabled {
                    label += " (off)";
                }
                if live > 0 {
                    label += &format!("  ({live} live)");
                }
                if ui.selectable_label(f.selected == i, label).clicked() {
                    f.selected = i;
                }
            }
            ui.separator();
            if ui.button("+ New team").clicked() {
                f.store.teams.push(Team::new(&format!("Team {}", f.store.teams.len() + 1)));
                f.selected = f.store.teams.len() - 1;
            }
            if ui.button("Import team…").clicked()
                && let Some(path) = pick_file(false, "")
            {
                f.status = match f.store.import(&path) {
                    Ok(i) => {
                        f.selected = i;
                        format!("Imported {}", f.store.teams[i].name)
                    }
                    Err(e) => format!("Import failed: {e}"),
                };
            }
            ui.allocate_rect(ui.available_rect_before_wrap(), egui::Sense::hover());
        })
        .response
        .rect
        .width();

    let task_panel = egui::Panel::right("tasks")
        .resizable(true)
        .default_size(270.0)
        .size_range(200.0..=480.0)
        .show(&mut root, |ui| {
            task_panel(ui, f);
            ui.allocate_rect(ui.available_rect_before_wrap(), egui::Sense::hover());
        })
        .response
        .rect
        .width();

    // Capped so a long line in the chat can never push the panel over the scene.
    let right = egui::Panel::right("details")
        .resizable(true)
        .default_size(470.0)
        .size_range(340.0..=760.0)
        .show(&mut root, |ui| {
            details_panel(ui, f, &mut scene.messages);
            ui.allocate_rect(ui.available_rect_before_wrap(), egui::Sense::hover());
        })
        .response
        .rect
        .width();

    // The Bevy scene fills what the panels leave free.
    let scale = window.scale_factor();
    let pos = UVec2::new((left * scale) as u32, (top * scale) as u32);
    let used = pos + UVec2::new(((right + task_panel) * scale) as u32, 0);
    let size = UVec2::new(window.physical_width().saturating_sub(used.x).max(1), window.physical_height().saturating_sub(used.y).max(1));
    camera.viewport = Some(Viewport { physical_position: pos, physical_size: size, ..default() });

    let scene_rect = egui::Rect::from_min_size(egui::pos2(left, top), egui::vec2(size.x as f32 / scale, size.y as f32 / scale));
    legend(&ctx, scene_rect, f.store.teams.is_empty());
    notifications(&ctx, scene_rect, f);
    if let Some(team) = f.store.teams.get_mut(f.selected) {
        root.interact(scene_rect, egui::Id::new("scene"), egui::Sense::click())
            .context_menu(|ui| scene_menu(ui, team, f.menu_on.as_deref(), &mut f.copied, &mut f.store.theme));
    }

    scene.nodes.clear();
    scene.theme = f.store.theme;
    scene.selected = if f.view == View::Chat { f.chat_with.clone() } else { String::new() };
    if let Some(team) = f.store.teams.get(f.selected) {
        let sessions: Vec<&live::Session> = f.sessions.iter().filter(|s| uses_team(&f.store, s, &team.name)).collect();
        scene.nodes = node_specs(team, &f.running, &sessions, &f.tasks);
        scene.messages.extend(delivered.into_iter().filter(|(t, _, _)| *t == team.name).map(|(_, from, to)| (from, to)));
    }

    f.autosave();
    Ok(())
}

fn legend(ctx: &egui::Context, rect: egui::Rect, empty: bool) {
    let items = [
        ("not started", Color32::from_rgb(82, 87, 102)),
        ("starting", Color32::from_rgb(102, 158, 242)),
        ("working", Color32::from_rgb(77, 209, 122)),
        ("idle", IDLE),
        ("stopped", FAILED),
    ];
    // Painted dots: the default egui font has no bullet glyph.
    egui::Area::new("legend".into())
        .fixed_pos(rect.left_bottom() + egui::vec2(14.0, -22.0 * items.len() as f32 - 8.0))
        .interactable(false)
        .show(ctx, |ui| {
            for (label, color) in items {
                ui.horizontal(|ui| {
                    let (dot, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                    ui.painter().circle_filled(dot.center(), 4.5, color);
                    ui.weak(label);
                });
            }
        });
    egui::Area::new("hint".into())
        .fixed_pos(rect.left_top() + egui::vec2(14.0, 10.0))
        .interactable(false)
        .show(ctx, |ui| {
            ui.weak(if empty { "Create a team with \"+ New team\"." } else { "Click an agent to chat with it. Scroll to zoom, drag to move." });
        });
}

/// The agents' pending questions, then their notifications newest first, in the scene's top-right corner.
fn notifications(ctx: &egui::Context, rect: egui::Rect, f: &mut Factory) {
    let questions: Vec<_> = f.running.iter().flat_map(|((team, agent), a)| a.questions().into_iter().map(move |q| (team, agent, a, q))).collect();
    if f.notifications.is_empty() && questions.is_empty() {
        return;
    }
    let list = &mut f.notifications;
    let mut dismiss = None;
    egui::Area::new("notifications".into())
        .anchor(egui::Align2::RIGHT_TOP, egui::vec2(rect.right() - ctx.content_rect().right() - 10.0, rect.top() + 10.0))
        .show(ctx, |ui| {
            ui.set_max_width(320.0);
            for (team, agent, a, q) in &questions {
                if q.new {
                    let first = q.input["questions"][0]["question"].as_str().unwrap_or("");
                    let _ = std::process::Command::new("notify-send")
                        .args(["--app-name=Claude Factory", "--urgency=critical", &format!("{agent} ({team}) asks"), first])
                        .spawn();
                }
                let picks = f.answers.entry(q.request_id.clone()).or_default();
                if let Some(answers) = question_card(ui, agent, &q.input, picks) {
                    a.answer(&q.request_id, answers);
                    f.answers.remove(&q.request_id);
                }
            }
            for (i, (team, text)) in list.iter().enumerate().rev() {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.strong(team);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("✕").on_hover_text("Dismiss").clicked() {
                                dismiss = Some(i);
                            }
                        });
                    });
                    ui.label(text);
                });
            }
            if list.len() > 1 && ui.small_button("Dismiss all").clicked() {
                dismiss = Some(usize::MAX);
            }
        });
    match dismiss {
        Some(usize::MAX) => list.clear(),
        Some(i) => _ = list.remove(i),
        None => {}
    }
}

/// One AskUserQuestion: every question with its options and an "Other" field; returns the answers once submitted.
fn question_card(ui: &mut egui::Ui, agent: &str, input: &serde_json::Value, picks: &mut Vec<(Vec<bool>, String)>) -> Option<serde_json::Map<String, serde_json::Value>> {
    let questions = input["questions"].as_array().map(Vec::as_slice).unwrap_or_default();
    picks.resize_with(questions.len(), Default::default);
    let mut submit = false;
    egui::Frame::popup(ui.style()).stroke(egui::Stroke::new(1.5, Color32::from_rgb(230, 180, 80))).show(ui, |ui| {
        ui.strong(format!("{agent} asks"));
        for (q, (ticked, other)) in questions.iter().zip(picks.iter_mut()) {
            let options = q["options"].as_array().map(Vec::as_slice).unwrap_or_default();
            ticked.resize(options.len(), false);
            let multi = q["multiSelect"] == true;
            ui.separator();
            ui.label(q["question"].as_str().unwrap_or(""));
            for (i, o) in options.iter().enumerate() {
                let label = o["label"].as_str().unwrap_or("");
                if ui.selectable_label(ticked[i], label).on_hover_text(o["description"].as_str().unwrap_or("")).clicked() {
                    if !multi {
                        ticked.iter_mut().for_each(|t| *t = false);
                        other.clear();
                    }
                    ticked[i] = !ticked[i] || !multi;
                }
            }
            if ui.add(egui::TextEdit::singleline(other).hint_text("Other…")).changed() && !multi && !other.is_empty() {
                ticked.iter_mut().for_each(|t| *t = false);
            }
        }
        let ready = picks.iter().all(|(t, o)| t.contains(&true) || !o.trim().is_empty());
        submit = ui.add_enabled(ready, egui::Button::new("Answer")).clicked();
    });
    submit.then(|| question_answers(questions, picks))
}

/// The AskUserQuestion `answers` map: question text to the ticked labels and the "Other" text, comma-separated.
fn question_answers(questions: &[serde_json::Value], picks: &[(Vec<bool>, String)]) -> serde_json::Map<String, serde_json::Value> {
    questions
        .iter()
        .zip(picks)
        .map(|(q, (ticked, other))| {
            let mut chosen: Vec<&str> = q["options"].as_array().into_iter().flatten().zip(ticked).filter(|(_, t)| **t).filter_map(|(o, _)| o["label"].as_str()).collect();
            if !other.trim().is_empty() {
                chosen.push(other.trim());
            }
            (q["question"].as_str().unwrap_or("").to_string(), chosen.join(", ").into())
        })
        .collect()
}

fn details_panel(ui: &mut egui::Ui, f: &mut Factory, messages: &mut Vec<(String, String)>) {
    if f.store.teams.is_empty() {
        ui.label("Create a team with \"+ New team\".");
        return;
    }
    f.selected = f.selected.min(f.store.teams.len() - 1);
    ui.horizontal(|ui| {
        ui.selectable_value(&mut f.view, View::Design, egui::RichText::new("Design").heading());
        ui.selectable_value(&mut f.view, View::Chat, egui::RichText::new("Chat").heading());
    });
    ui.separator();
    let mut delete = false;
    let mut export = false;
    let sessions: Vec<live::Session> = f.sessions.iter().filter(|s| uses_team(&f.store, s, &f.store.teams[f.selected].name)).cloned().collect();
    let team = &mut f.store.teams[f.selected];
    match f.view {
        View::Design => {
            egui::ScrollArea::vertical().show(ui, |ui| {
                team_editor(ui, team, &mut delete, &mut export);
                ui.add_space(8.0);
                members_editor(ui, team);
                ui.add_space(8.0);
                routines_editor(ui, team);
                ui.add_space(8.0);
                live_sessions(ui, &sessions.iter().collect::<Vec<_>>());
                ui.add_space(8.0);
                prompt_preview(ui, team);
            });
        }
        View::Chat => {
            let chat = Chat { running: &mut f.running, tags: &mut f.tags, chat_with: &mut f.chat_with, draft: &mut f.draft, attachments: &mut f.attachments };
            chat_view(ui, team, chat, &mut f.history, &f.store.history, messages)
        }
    }
    if export && let Some(path) = pick_file(true, &format!("{}.json", f.store.teams[f.selected].name)) {
        f.status = match Store::export(&f.store.teams[f.selected], &path) {
            Ok(()) => format!("Exported to {}", path.display()),
            Err(e) => format!("Export failed: {e}"),
        };
    }
    if delete {
        let name = f.store.teams[f.selected].name.clone();
        f.running.retain(|(t, _), _| *t != name);
        f.store.teams.remove(f.selected);
        f.selected = f.selected.saturating_sub(1);
    }
}

fn state_of_agent(status: Status) -> NodeState {
    match status {
        Status::Starting => NodeState::Starting,
        Status::Working => NodeState::Working,
        Status::Idle => NodeState::Idle,
        Status::Exited => NodeState::Failed,
    }
}

/// What a terminal session's subagent status (reported by the mod) looks like in the scene.
fn state_of_live(status: &str) -> NodeState {
    match status {
        "running" | "pending" => NodeState::Working,
        "idle" | "waiting" => NodeState::Idle,
        "completed" => NodeState::Done,
        _ => NodeState::Failed,
    }
}

/// The scene's nodes, lead first: what the app runs, else what the mod reports from a terminal session.
fn node_specs(team: &Team, running: &Running, sessions: &[&live::Session], tasks: &[tasks::Task]) -> Vec<NodeSpec> {
    // The task an agent is on reads better than its bare status.
    let current = |name: &str| tasks.iter().find(|t| t.in_progress() && t.owner.as_deref().is_some_and(|o| o.eq_ignore_ascii_case(name)));
    let thumbnail = |name: &str| running.get(&(team.name.clone(), name.to_string())).and_then(|a| a.thumbnail());
    let state = |name: &str, live: Option<NodeState>| -> (NodeState, Option<String>) {
        if let Some(agent) = running.get(&(team.name.clone(), name.to_string())) {
            let s = agent.status();
            return (state_of_agent(s), Some(s.label().into()));
        }
        match live {
            Some(s) => (s, Some(format!("{s:?}").to_lowercase())),
            None => (NodeState::Off, None),
        }
    };
    let subtitle = |role: &str, status: Option<String>| match status {
        // Bevy's built-in font has no middle dot.
        Some(s) => format!("{role} - {s}"),
        None => role.to_string(),
    };
    let lead = team.lead_name();
    let (lead_state, lead_status) = state(&lead, (!sessions.is_empty()).then_some(NodeState::Working));
    let mut specs = vec![NodeSpec {
        thumbnail: thumbnail(&lead),
        key: lead,
        title: "Team lead".into(),
        subtitle: subtitle(&team.name, lead_status),
        state: lead_state,
        is_lead: true,
    }];
    for m in &team.members {
        let live = sessions.iter().find_map(|s| s.agent(&m.name)).map(|a| state_of_live(&a.status));
        let (st, mut status) = state(&m.name, live);
        if let Some(task) = current(&m.name) {
            status = Some(format!("#{} {}", task.id, ellipsize(&task.subject, 22)));
        }
        specs.push(NodeSpec { key: m.name.clone(), title: m.name.clone(), subtitle: subtitle(m.role.label(), status), state: st, is_lead: false, thumbnail: thumbnail(&m.name) });
    }
    specs
}

fn ellipsize(text: &str, max: usize) -> String {
    if text.chars().count() <= max { text.into() } else { format!("{}…", text.chars().take(max - 1).collect::<String>()) }
}

fn task_panel(ui: &mut egui::Ui, f: &mut Factory) {
    let Some(team) = f.store.teams.get(f.selected) else { return };
    // A right panel that lays out wider than itself is drawn under the details panel, cut on the left: wrap
    // everything, rows and long paths included.
    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
    let list_id = team.task_list_id();
    ui.horizontal(|ui| {
        ui.heading("Tasks");
        let done = f.tasks.iter().filter(|t| t.done()).count();
        if !f.tasks.is_empty() {
            ui.weak(format!("{done}/{} done", f.tasks.len()));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if !f.tasks.is_empty() && ui.small_button("Clear").on_hover_text("Delete every task of this team's list").clicked() {
                tasks::clear(&list_id);
                f.tasks.clear();
            }
        });
    });
    ui.weak(format!("Shared list ~/.claude/tasks/{list_id}"));
    ui.separator();
    if f.tasks.is_empty() {
        ui.label("No tasks yet. The lead plans the work here once you give it something to do.");
        return;
    }
    egui::ScrollArea::vertical().id_salt("tasks").show(ui, |ui| {
        // What is being worked on first, then what is waiting, then what is done.
        let order = |t: &tasks::Task| if t.in_progress() { 0 } else if t.done() { 2 } else { 1 };
        let mut sorted: Vec<&tasks::Task> = f.tasks.iter().collect();
        sorted.sort_by_key(|t| order(t));
        let time = ui.input(|i| i.time);
        for task in sorted {
            let blocked = !task.blocked_by.is_empty()
                && task.blocked_by.iter().any(|b| f.tasks.iter().any(|t| &t.id == b && !t.done()));
            let (icon, color) = if task.in_progress() {
                ("", Color32::from_rgb(77, 209, 122))
            } else if task.done() {
                ("✔", Color32::from_rgb(98, 133, 235))
            } else if blocked {
                ("⏸", Color32::GRAY)
            } else {
                ("○", IDLE)
            };
            let frame = egui::Frame::group(ui.style()).fill(if task.in_progress() {
                Color32::from_rgba_unmultiplied(77, 209, 122, 22)
            } else {
                Color32::TRANSPARENT
            });
            frame.show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    if task.in_progress() {
                        // A pulsing painted dot (the egui font has no bullet) marks the task being worked on now.
                        let pulse = (0.55 + 0.45 * (time * 4.0).sin()) as f32;
                        let (dot, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                        ui.painter().circle_filled(dot.center(), 3.5 + 1.5 * pulse, color.gamma_multiply(0.5 + 0.5 * pulse));
                    } else {
                        ui.colored_label(color, icon);
                    }
                    let subject = egui::RichText::new(format!("#{} {}", task.id, task.subject));
                    ui.label(if task.done() { subject.weak().strikethrough() } else { subject.strong() });
                });
                let mut meta = Vec::new();
                if let Some(owner) = &task.owner {
                    meta.push(if task.in_progress() { format!("{owner} is on it") } else { owner.clone() });
                }
                if blocked {
                    meta.push(format!("waits on #{}", task.blocked_by.join(", #")));
                }
                if !meta.is_empty() {
                    ui.weak(meta.join(" · "));
                }
                if task.in_progress() && task.description.trim() != task.subject.trim() && !task.description.trim().is_empty() {
                    ui.label(egui::RichText::new(ellipsize(task.description.trim(), 160)).small());
                }
            });
        }
    });
}

fn uses_team(store: &Store, session: &live::Session, team: &str) -> bool {
    session.is_live() && store.team_for_dir(&session.cwd).is_some_and(|t| t.name == team)
}

/// SessionStart hook: reads the hook's JSON on stdin and prints the team prompt for its cwd.
/// Prints nothing on any problem, so a broken setup never blocks a Claude session.
fn session_start() {
    // The app's own agents already carry their member prompt; the lead prompt would contradict it.
    if std::env::var_os(runner::AGENT_ENV).is_some() {
        return;
    }
    let mut input = String::new();
    let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut input);
    let cwd = serde_json::from_str::<serde_json::Value>(&input)
        .ok()
        .and_then(|v| v["cwd"].as_str().map(String::from))
        .or_else(|| std::env::current_dir().ok().map(|d| d.display().to_string()))
        .unwrap_or_default();
    if let Some(team) = Store::load().team_for_dir(&cwd) {
        let output = serde_json::json!({
            "hookSpecificOutput": { "hookEventName": "SessionStart", "additionalContext": prompt::render(team) }
        });
        println!("{output}");
    }
}

/// A native file dialog through zenity or kdialog, so the app needs no dialog crate.
// shortcut: blocks the UI until the dialog closes and needs zenity or kdialog installed; move to a portal/rfd if that bites.
fn pick_file(save: bool, default_name: &str) -> Option<std::path::PathBuf> {
    use std::process::Command;
    let start = dirs::home_dir().unwrap_or_default().join(default_name).display().to_string();
    let mut zenity = Command::new("zenity");
    zenity.args(["--file-selection", "--file-filter=*.json", "--filename", &start]);
    if save {
        zenity.args(["--save", "--confirm-overwrite"]);
    }
    let mut kdialog = Command::new("kdialog");
    kdialog.args([if save { "--getsavefilename" } else { "--getopenfilename" }, &start, "*.json"]);
    // Exit code 1 is "cancelled", so only a missing program falls through to the next one.
    let out = zenity.output().or_else(|_| kdialog.output()).ok()?;
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !path.is_empty()).then(|| path.into())
}

fn team_editor(ui: &mut egui::Ui, team: &mut Team, delete: &mut bool, export: &mut bool) {
    egui::Grid::new("team").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Name");
        ui.horizontal(|ui| {
            ui.text_edit_singleline(&mut team.name);
            ui.checkbox(&mut team.enabled, "Send at session start");
            if ui.button("Export…").on_hover_text("Save this team to a JSON file").clicked() {
                *export = true;
            }
            if ui.button("Delete team").clicked() {
                *delete = true;
            }
        });
        ui.end_row();
        ui.label("Directory");
        ui.add(
            egui::TextEdit::singleline(&mut team.directory)
                .hint_text("Only sessions started in this folder get the team (empty = every session)")
                .desired_width(f32::INFINITY),
        );
        ui.end_row();
        ui.label("Team rules");
        ui.add(
            egui::TextEdit::multiline(&mut team.rules)
                .hint_text("e.g. This team works on MyApp. All developer work is tested by QA. Never create PRs.")
                .desired_rows(4)
                .desired_width(f32::INFINITY),
        );
        ui.end_row();
    });
}

/// Right-click menu of the scene: copy or remove the agent under the cursor, paste or add one.
fn scene_menu(ui: &mut egui::Ui, team: &mut Team, on: Option<&str>, copied: &mut Option<Member>, theme: &mut scene::Theme) {
    // None for the lead and empty canvas: neither can be copied or removed.
    let member = on.and_then(|key| team.members.iter().position(|m| m.name == key));
    if let Some(i) = member {
        if ui.button("Copy").clicked() {
            *copied = Some(team.members[i].clone());
        }
        if ui.button("Remove agent").clicked() {
            team.members.remove(i);
        }
        ui.separator();
    }
    if ui.add_enabled(copied.is_some(), egui::Button::new("Paste")).clicked()
        && let Some(m) = copied.clone()
    {
        add_member(team, m);
    }
    ui.menu_button("Add agent", |ui| {
        for role in Role::ALL {
            if ui.button(role.label()).clicked() {
                add_member(team, Member::new(role.label(), role));
            }
        }
    });
    if on.is_none() {
        ui.menu_button("Theme", |ui| {
            for t in scene::Theme::ALL {
                ui.radio_value(theme, t, t.label());
            }
        });
    }
}

/// Appends `m`, numbering its name ("Developer 2") if another agent already has it.
fn add_member(team: &mut Team, mut m: Member) {
    let taken = |team: &Team, name: &str| team.members.iter().any(|o| o.name == name) || name == team.lead_name();
    if taken(team, &m.name) {
        let base = m.name.clone();
        m.name = (2..).map(|n| format!("{base} {n}")).find(|n| !taken(team, n)).unwrap();
    }
    team.members.push(m);
}

fn members_editor(ui: &mut egui::Ui, team: &mut Team) {
    ui.heading("Agents");
    let mut remove = None;
    let mut swap = None;
    let last = team.members.len().saturating_sub(1);
    for (i, m) in team.members.iter_mut().enumerate() {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Name");
                ui.add(egui::TextEdit::singleline(&mut m.name).desired_width(140.0));
                ui.label("Role");
                let before = m.role;
                egui::ComboBox::from_id_salt(("role", i)).selected_text(m.role.label()).show_ui(ui, |ui| {
                    for r in Role::ALL {
                        ui.selectable_value(&mut m.role, r, r.label());
                    }
                });
                // Swap in the new role's default text unless the user wrote their own.
                if m.role != before && m.instructions == before.default_instructions() {
                    m.instructions = m.role.default_instructions().into();
                }
                ui.label("Model");
                let shown = if m.model.is_empty() { "lead's model".to_string() } else { m.model.clone() };
                egui::ComboBox::from_id_salt(("model", i)).selected_text(shown).show_ui(ui, |ui| {
                    for model in MODELS {
                        ui.selectable_value(&mut m.model, model.to_string(), if model.is_empty() { "lead's model" } else { model });
                    }
                });
                // Members are laid out left to right in this order, so these move the node in the graph.
                if ui.add_enabled(i > 0, egui::Button::new("◀")).on_hover_text("Move left in the graph").clicked() {
                    swap = Some(i - 1);
                }
                if ui.add_enabled(i < last, egui::Button::new("▶")).on_hover_text("Move right in the graph").clicked() {
                    swap = Some(i);
                }
                if ui.button("Remove").clicked() {
                    remove = Some(i);
                }
            });
            ui.add(
                egui::TextEdit::multiline(&mut m.instructions)
                    .hint_text("What this agent does and must not do")
                    .desired_rows(2)
                    .desired_width(f32::INFINITY),
            );
        });
    }
    if let Some(i) = swap {
        team.members.swap(i, i + 1);
    }
    if let Some(i) = remove {
        team.members.remove(i);
    }
    ui.horizontal(|ui| {
        for role in Role::ALL {
            if ui.button(format!("+ {}", role.label())).clicked() {
                add_member(team, Member::new(&format!("{} {}", role.label(), team.members.len() + 1), role));
            }
        }
    });
}

fn routines_editor(ui: &mut egui::Ui, team: &mut Team) {
    use team::{Routine, Schedule};
    ui.heading("Routines");
    ui.weak("Prompts sent to the lead on a schedule, while the app and the lead are running. Missed runs are skipped.");
    let mut remove = None;
    for (i, r) in team.routines.iter_mut().enumerate() {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Name");
                ui.add(egui::TextEdit::singleline(&mut r.name).desired_width(140.0));
                ui.checkbox(&mut r.enabled, "Enabled");
                ui.weak(if r.last_run == 0 { "never run".to_string() } else { format!("last run {}", history::format_time(r.last_run)) });
                if ui.button("Remove").clicked() {
                    remove = Some(i);
                }
            });
            ui.horizontal(|ui| {
                let every = matches!(r.schedule, Schedule::Every { .. });
                egui::ComboBox::from_id_salt(("schedule", i)).selected_text(if every { "Every" } else { "Daily at" }).show_ui(ui, |ui| {
                    if ui.selectable_label(every, "Every").clicked() && !every {
                        r.schedule = Schedule::Every { n: 1, hours: true };
                    }
                    if ui.selectable_label(!every, "Daily at").clicked() && every {
                        r.schedule = Schedule::Daily { hour: 9, minute: 0, days: [true, true, true, true, true, false, false] };
                    }
                });
                match &mut r.schedule {
                    Schedule::Every { n, hours } => {
                        ui.add(egui::DragValue::new(n).range(1..=999));
                        egui::ComboBox::from_id_salt(("unit", i)).selected_text(if *hours { "hours" } else { "minutes" }).show_ui(ui, |ui| {
                            ui.selectable_value(hours, false, "minutes");
                            ui.selectable_value(hours, true, "hours");
                        });
                    }
                    Schedule::Daily { hour, minute, days } => {
                        ui.add(egui::DragValue::new(hour).range(0..=23).custom_formatter(|v, _| format!("{v:02}")));
                        ui.label(":");
                        ui.add(egui::DragValue::new(minute).range(0..=59).custom_formatter(|v, _| format!("{v:02}")));
                        for (day, on) in ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"].iter().zip(days.iter_mut()) {
                            ui.checkbox(on, *day);
                        }
                    }
                }
            });
            ui.add(
                egui::TextEdit::multiline(&mut r.prompt)
                    .hint_text("What the lead should do each time, e.g. Check the open PRs and report their CI status.")
                    .desired_rows(2)
                    .desired_width(f32::INFINITY),
            );
        });
    }
    if let Some(i) = remove {
        team.routines.remove(i);
    }
    if ui.button("+ Routine").clicked() {
        team.routines.push(Routine::new(&format!("Routine {}", team.routines.len() + 1)));
    }
}

fn live_sessions(ui: &mut egui::Ui, sessions: &[&live::Session]) {
    ui.heading("Live sessions");
    if sessions.is_empty() {
        ui.label("No running Claude session is using this team.");
    }
    for s in sessions {
        ui.collapsing(format!("{}  ({})", &s.session_id[..8.min(s.session_id.len())], s.cwd), |ui| {
            if s.agents.is_empty() {
                ui.label("No agents spawned yet.");
            }
            for a in &s.agents {
                ui.label(format!("{}  [{}]  {}  {}", a.name.as_deref().unwrap_or("(unnamed)"), a.status, a.kind, a.description));
            }
        });
    }
}

fn prompt_preview(ui: &mut egui::Ui, team: &Team) {
    let mut text = prompt::render(team);
    ui.horizontal(|ui| {
        ui.heading("Prompt sent at session start");
        if ui.button("Copy").clicked() {
            ui.ctx().copy_text(text.clone());
        }
    });
    ui.add(egui::TextEdit::multiline(&mut text).code_editor().interactive(false).desired_width(f32::INFINITY));
}

/// The chat panel's share of the app state.
struct Chat<'a> {
    running: &'a mut Running,
    tags: &'a mut HashMap<String, String>,
    chat_with: &'a mut String,
    draft: &'a mut String,
    attachments: &'a mut Vec<std::path::PathBuf>,
}

fn chat_view(ui: &mut egui::Ui, team: &mut Team, chat: Chat, hv: &mut HistoryView, past: &[PastSession], messages: &mut Vec<(String, String)>) {
    let Chat { running, tags, chat_with, draft, attachments } = chat;
    let team_running = running.keys().any(|(t, _)| *t == team.name);
    ui.horizontal(|ui| {
        if team_running {
            if ui.button("Stop team").clicked() {
                running.retain(|(t, _), _| *t != team.name);
            }
        } else if ui.add_enabled(!team.members.is_empty(), egui::Button::new("Start team")).clicked() {
            start_team(team, running, tags);
        }
        ui.label("Permissions");
        ui.add_enabled_ui(!team_running, |ui| {
            egui::ComboBox::from_id_salt("perm").selected_text(&team.permission_mode).show_ui(ui, |ui| {
                for mode in team::PERMISSION_MODES {
                    ui.selectable_value(&mut team.permission_mode, mode.to_string(), mode);
                }
            });
        })
        .response
        .on_hover_text("The agents run without a terminal, so nobody can answer a permission prompt: anything the mode does not allow is denied.");
        ui.add(egui::Label::new(egui::RichText::new(format!("in {}", team.working_dir())).weak()).truncate());
    });

    let lead = team.lead_name();
    if *chat_with != lead && team.members.iter().all(|m| m.name != *chat_with) {
        *chat_with = lead.clone();
    }

    ui.horizontal(|ui| {
        ui.selectable_value(chat_with, lead.clone(), "Lead");
        for m in &team.members {
            ui.selectable_value(chat_with, m.name.clone(), &m.name);
        }
        ui.separator();
        if ui.toggle_value(&mut hv.open, "🕘 History").on_hover_text("This agent's past conversations").clicked() {
            hv.for_key = None;
        }
    });
    let key = (team.name.clone(), chat_with.clone());
    if hv.open {
        let live_id = running.get(&key).and_then(|a| a.session_id());
        if let Some((summary, entries)) = history_view(ui, team, &key.1, past, hv, live_id) {
            reopen(team, &key.1, &summary.id, entries, running, tags);
            hv.open = false;
            hv.viewing = None;
        }
        return;
    }
    let Some(agent) = running.get_mut(&key) else {
        ui.label(if team_running {
            "This agent is not running. Stop and start the team to add it."
        } else {
            "Start the team to talk to its agents."
        });
        return;
    };

    let input_height = if attachments.is_empty() { 70.0 } else { 100.0 };
    egui::ScrollArea::vertical()
        .id_salt(("chat", &key.1))
        .stick_to_bottom(true)
        .auto_shrink(false)
        .max_height(ui.available_height() - input_height)
        .show(ui, |ui| {
            keep_label_selection(ui, |ui| {
                for entry in agent.entries() {
                    chat_entry(ui, &key.1, &entry, tags.get(&team.name).map_or("", |t| t.as_str()));
                }
            });
            if agent.status() == Status::Working {
                ui.weak(format!("{} is working…", key.1));
            }
        });

    ui.separator();
    let temp = std::env::temp_dir();
    let mut removed = None;
    ui.horizontal_wrapped(|ui| {
        for (i, path) in attachments.iter().enumerate() {
            let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
            ui.label(format!("📎 {name}")).on_hover_text(path.display().to_string());
            if ui.small_button("✕").on_hover_text("Remove this attachment").clicked() {
                removed = Some(i);
            }
        }
    });
    if let Some(i) = removed {
        attach::remove(attachments, i, &temp);
    }
    ui.horizontal(|ui| {
        let edit_id = ui.make_persistent_id("chat_input");
        paste_attachments(ui, edit_id, attachments, &temp);
        let picked = skill_popup_keys(ui, edit_id, draft, &agent.commands());
        let edit = egui::TextEdit::multiline(draft)
            .id(edit_id)
            .hint_text(format!("Message {} (Enter to send, Shift+Enter for a new line, / for skills, Ctrl+V to attach)", key.1))
            .desired_rows(2)
            .desired_width(ui.available_width() - 70.0)
            .return_key(KeyboardShortcut::new(Modifiers::SHIFT, Key::Enter));
        let response = ui.add(edit);
        response.context_menu(|ui| {
            let selected = egui::TextEdit::load_state(ui.ctx(), edit_id)
                .and_then(|s| s.cursor.char_range())
                .map(|r| r.slice_str(draft).to_string())
                .filter(|t| !t.is_empty());
            if ui.add_enabled(selected.is_some(), egui::Button::new("Copy")).clicked()
                && let Some(t) = selected
            {
                ui.ctx().copy_text(t);
            }
            if ui.button("Paste").clicked() {
                // Done next frame by `paste_attachments`, once the input has its focus back.
                ui.memory_mut(|m| m.request_focus(edit_id));
                ui.data_mut(|d| d.insert_temp(edit_id.with("menu_paste"), true));
            }
        });
        skill_popup(ui, &response, draft, picked);
        let enter = response.has_focus() && ui.input(|i| i.key_pressed(Key::Enter) && !i.modifiers.shift);
        if (ui.button("Send").clicked() || enter) && !(draft.trim().is_empty() && attachments.is_empty()) {
            agent.send(draft.trim(), attachments);
            messages.push((scene::YOU.to_string(), key.1.clone()));
            draft.clear();
            attachments.clear();
            response.request_focus();
        }
    });
}

/// On Ctrl+V or the context menu's Paste in the chat input, turns copied files or a raw image into attachments
/// instead of pasted text. Runs before the text field, so it can drop the text egui would otherwise insert (file
/// managers also put the files' URIs or paths there).
fn paste_attachments(ui: &mut egui::Ui, edit_id: egui::Id, attachments: &mut Vec<std::path::PathBuf>, temp: &std::path::Path) {
    let from_menu = ui.data_mut(|d| d.remove_temp::<bool>(edit_id.with("menu_paste"))).unwrap_or(false);
    // The menu has no key event behind it: feed the clipboard text in as the paste Ctrl+V would bring.
    if from_menu && let Some(t) = arboard::Clipboard::new().ok().and_then(|mut cb| cb.get_text().ok()) {
        ui.input_mut(|i| i.events.push(egui::Event::Paste(t)));
    }
    let pasting = from_menu || (ui.memory(|m| m.has_focus(edit_id)) && ui.input(|i| i.modifiers.command && i.key_pressed(Key::V)));
    if !pasting {
        return;
    }
    let text = ui.input(|i| {
        i.events.iter().find_map(|e| match e {
            egui::Event::Text(t) | egui::Event::Paste(t) => Some(t.clone()),
            _ => None,
        })
    });
    let Some(paste) = attach::read_clipboard().or(text.map(attach::Paste::Text)) else { return };
    match attach::attachments(paste, temp) {
        Ok(files) if files.is_empty() => {}
        Ok(files) => {
            attachments.extend(files);
            ui.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Text(_) | egui::Event::Paste(_))));
        }
        Err(e) => eprintln!("Could not attach the pasted image: {e}"),
    }
}

/// The "/word" being typed at the end of the draft, as (byte offset of the "/", lowercase query), when the "/"
/// starts the draft or follows whitespace.
/// shortcut: looks at the end of the draft, not the cursor; track the cursor if people edit mid-text.
fn slash_query(draft: &str) -> Option<(usize, String)> {
    let word = draft.rsplit(char::is_whitespace).next()?;
    Some((draft.len() - word.len(), word.strip_prefix('/')?.to_lowercase()))
}

/// Popup state kept in egui memory between frames.
#[derive(Clone, Default)]
struct SkillPopup {
    /// Commands matching the current query, prefix matches first.
    matches: Vec<(String, String)>,
    selected: usize,
    /// The draft as it was when Esc closed the popup; it stays closed until the draft changes.
    dismissed: Option<String>,
    /// The draft last frame, to reset the selection when the query changes.
    last_draft: String,
    open: bool,
    /// The selection moved with the keyboard this frame: bring it into view.
    scroll: bool,
}

/// Runs before the chat input so the arrows, Tab, Enter and Esc go to the popup instead of the text field.
/// Returns the command picked with the keyboard.
fn skill_popup_keys(ui: &mut egui::Ui, edit_id: egui::Id, draft: &str, commands: &[(String, String)]) -> Option<String> {
    let popup_id = edit_id.with("skills");
    let mut p: SkillPopup = ui.data(|d| d.get_temp(popup_id)).unwrap_or_default();
    if p.last_draft != draft {
        p.selected = 0;
        p.dismissed = None;
        p.last_draft = draft.to_string();
    }
    p.matches = match slash_query(draft) {
        Some((_, q)) => {
            let mut m: Vec<_> = commands.iter().filter(|(name, _)| name.to_lowercase().contains(&q)).cloned().collect();
            m.sort_by_key(|(name, _)| !name.to_lowercase().starts_with(&q));
            m
        }
        None => Vec::new(),
    };
    let focused = ui.memory(|m| m.has_focus(edit_id));
    p.open = focused && !p.matches.is_empty() && p.dismissed.is_none();
    let mut picked = None;
    p.scroll = false;
    if p.open {
        let n = p.matches.len();
        p.selected = p.selected.min(n - 1);
        ui.input_mut(|i| {
            if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                p.selected = (p.selected + 1) % n;
                p.scroll = true;
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                p.selected = (p.selected + n - 1) % n;
                p.scroll = true;
            }
            if i.consume_key(Modifiers::NONE, Key::Tab) || i.consume_key(Modifiers::NONE, Key::Enter) {
                picked = Some(p.matches[p.selected].0.clone());
            }
            if i.consume_key(Modifiers::NONE, Key::Escape) {
                p.dismissed = Some(draft.to_string());
                p.open = false;
            }
        });
        // egui already queued a Tab focus move at the start of the frame; cancel it so the cursor stays in the input.
        if picked.is_some() {
            ui.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
        }
    }
    ui.data_mut(|d| d.insert_temp(popup_id, p));
    picked
}

/// Draws the skill list above the chat input and inserts the picked command ("/name ") in place of the query.
fn skill_popup(ui: &mut egui::Ui, input: &egui::Response, draft: &mut String, mut picked: Option<String>) {
    let popup_id = input.id.with("skills");
    let p: SkillPopup = ui.data(|d| d.get_temp(popup_id)).unwrap_or_default();
    if p.open && picked.is_none() {
        egui::Area::new(popup_id)
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::LEFT_BOTTOM)
            .fixed_pos(input.rect.left_top())
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(input.rect.width());
                    egui::ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                        for (i, (name, description)) in p.matches.iter().enumerate() {
                            let line = description.lines().next().unwrap_or("");
                            let line: String = line.chars().take(100).collect();
                            let text = egui::RichText::new(format!("/{name}  {line}"));
                            let item = ui.add(egui::Button::selectable(i == p.selected, text));
                            if i == p.selected && p.scroll {
                                item.scroll_to_me(None);
                            }
                            if item.clicked() {
                                picked = Some(name.clone());
                            }
                        }
                    });
                });
            });
    }
    let (Some(name), Some((start, _))) = (picked, slash_query(draft)) else { return };
    draft.truncate(start);
    draft.push_str(&format!("/{name} "));
    // Put the cursor after the inserted command, and keep typing there after a click on the list.
    if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), input.id) {
        let end = egui::text::CCursor::new(draft.chars().count());
        state.cursor.set_char_range(Some(egui::text::CCursorRange::one(end)));
        state.store(ui.ctx(), input.id);
    }
    input.request_focus();
}

/// Copies each SendMessage an agent made into the recipient's chat, when the recipient is one of ours, and
/// returns them as (team, from, to) for the scene to animate.
/// A reply is addressed to the sender's inbox socket (`uds:/…/<pid>.sock`) rather than its name.
fn route_messages(running: &Running, tags: &HashMap<String, String>) -> Vec<(String, String, String)> {
    let mut delivered = Vec::new();
    for ((team, from), agent) in running {
        for (to, text) in agent.take_outbox() {
            let to = tags.get(team).map_or(to.clone(), |tag| untag(&to, tag));
            let pid = to.strip_suffix(".sock").and_then(|p| p.rsplit('/').next()).and_then(|p| p.parse::<u32>().ok());
            let target = running.get_key_value(&(team.clone(), to.clone())).or_else(|| {
                running.iter().find(|((t, _), a)| t == team && Some(a.pid()) == pid)
            });
            if let Some(((_, to_name), target)) = target {
                target.note_incoming(from, &text);
                delivered.push((team.clone(), from.clone(), to_name.clone()));
            }
        }
    }
    delivered
}

/// Session names must be unique on the machine: a leftover session or another team's "Developer 1" would
/// make SendMessage ambiguous, so every launch gets its own suffix.
fn new_tag() -> String {
    use std::hash::{BuildHasher, Hasher};
    format!("{:04x}", std::collections::hash_map::RandomState::new().build_hasher().finish() as u16)
}

fn start_team(team: &Team, running: &mut Running, tags: &mut HashMap<String, String>) {
    let tag = new_tag();
    let names = std::iter::once(team.lead_name()).chain(team.members.iter().map(|m| m.name.clone()));
    for name in names {
        if let Some(agent) = start_seat(team, &name, &tag, None) {
            running.insert((team.name.clone(), name), agent);
        }
    }
    tags.insert(team.name.clone(), tag);
}

/// Starts the lead or the member called `name`, on a new session or resuming `resume`.
fn start_seat(team: &Team, name: &str, tag: &str, resume: Option<&str>) -> Option<Agent> {
    // The lead runs on the default model; each member on its own pick.
    let (model, system_prompt) = if name == team.lead_name() {
        (String::new(), prompt::render_lead(team, tag))
    } else {
        let m = team.members.iter().find(|m| m.name == name)?;
        (m.model.clone(), prompt::render_member(team, m, tag))
    };
    let session_name = prompt::tagged(name.trim(), tag);
    let launch = runner::Launch {
        name: &session_name,
        cwd: &team.working_dir(),
        model: &model,
        permission_mode: &team.permission_mode,
        system_prompt,
        task_list_id: &team.task_list_id(),
        resume,
    };
    // Bevy redraws every frame, so the reader threads need no wake-up call.
    Agent::start(launch, || {}).inspect_err(|e| eprintln!("could not start {name}: {e}")).ok()
}

/// Restarts one agent on a past session, showing that session's conversation first. The rest of the team keeps
/// running; if it was stopped, only this agent starts.
fn reopen(team: &Team, name: &str, id: &str, earlier: Vec<Entry>, running: &mut Running, tags: &mut HashMap<String, String>) {
    let team_running = running.keys().any(|(t, _)| *t == team.name);
    let tag = tags.get(&team.name).filter(|_| team_running).cloned().unwrap_or_else(new_tag);
    let key = (team.name.clone(), name.to_string());
    // Stop the current session first: two sessions must not share the name.
    running.remove(&key);
    if let Some(agent) = start_seat(team, name, &tag, Some(id)) {
        agent.preload(earlier);
        running.insert(key, agent);
    }
    tags.insert(team.name.clone(), tag);
}

/// The History mode of the chat panel. Returns the conversation to reopen, with its entries, when asked.
fn history_view(
    ui: &mut egui::Ui,
    team: &Team,
    agent: &str,
    past: &[PastSession],
    hv: &mut HistoryView,
    live_id: Option<String>,
) -> Option<(history::Summary, Vec<Entry>)> {
    let key = (team.name.clone(), agent.to_string());
    if hv.for_key.as_ref() != Some(&key) {
        hv.list = history_list(team, agent, past);
        hv.for_key = Some(key);
        hv.viewing = None;
    }

    if let Some((summary, entries)) = &hv.viewing {
        let mut back = false;
        let mut reopen = None;
        let live = live_id.as_deref() == Some(summary.id.as_str());
        ui.horizontal(|ui| {
            back = ui.button("← Back").clicked();
            let button = ui
                .add_enabled(!live, egui::Button::new("Reopen"))
                .on_hover_text("Restart this agent on this conversation, so it continues where it left off")
                .on_disabled_hover_text("This conversation is the one running now");
            if button.clicked() {
                reopen = Some((summary.clone(), entries.clone()));
            }
            ui.weak(history::format_time(summary.started_at));
        });
        ui.separator();
        egui::ScrollArea::vertical().id_salt(("past", &summary.id)).auto_shrink(false).show(ui, |ui| {
            if entries.is_empty() {
                ui.weak("Nothing to show: the transcript is empty or unreadable.");
            }
            keep_label_selection(ui, |ui| {
                for entry in entries {
                    chat_entry(ui, agent, entry, "");
                }
            });
        });
        if back {
            hv.viewing = None;
        }
        return reopen;
    }

    ui.horizontal(|ui| {
        ui.weak(format!("{} past conversations of {agent}", hv.list.len()));
        if ui.small_button("⟳").on_hover_text("Refresh").clicked() {
            hv.for_key = None;
        }
    });
    if hv.list.is_empty() {
        ui.label("No past conversations yet.");
        return None;
    }
    let mut open = None;
    egui::ScrollArea::vertical().id_salt("past_list").auto_shrink(false).show(ui, |ui| {
        for s in &hv.list {
            let mark = if live_id.as_deref() == Some(s.id.as_str()) { "● " } else { "" };
            let preview = if s.preview.is_empty() { "(no messages)" } else { s.preview.as_str() };
            let text = format!("{mark}{}  {preview}", history::format_time(s.started_at));
            let row = ui.add_enabled(s.path.is_some(), egui::Button::selectable(false, text).truncate());
            if row.on_disabled_hover_text("Claude no longer has this transcript").clicked() {
                open = Some(s.clone());
            }
        }
    });
    if let Some(s) = open {
        let entries = s.path.as_deref().map(history::load).unwrap_or_default();
        hv.viewing = Some((s, entries));
    }
    None
}

/// The agent's sessions the app recorded, plus transcripts named after it in the team's folder, newest first.
fn history_list(team: &Team, agent: &str, past: &[PastSession]) -> Vec<history::Summary> {
    let cwd = team.working_dir();
    let mut list = history::discover(&cwd, agent);
    // Another team in the same folder may have an agent of the same name: trust the record over the name.
    list.retain(|s| !past.iter().any(|p| p.id == s.id && (p.team != team.name || p.agent != agent)));
    for p in past.iter().filter(|p| p.team == team.name && p.agent == agent) {
        match list.iter_mut().find(|s| s.id == p.id) {
            Some(s) if !p.preview.is_empty() => s.preview = p.preview.clone(),
            Some(_) => {}
            None => list.push(history::Summary {
                id: p.id.clone(),
                started_at: p.started_at,
                preview: p.preview.clone(),
                path: history::find(&cwd, &p.id),
            }),
        }
    }
    list.sort_by_key(|s| std::cmp::Reverse(s.started_at));
    list
}

/// Removes a launch tag from a session name, or from text that mentions session names.
fn untag(text: &str, tag: &str) -> String {
    text.replace(&format!("-{tag}"), "")
}

fn chat_entry(ui: &mut egui::Ui, agent: &str, entry: &Entry, tag: &str) {
    // Agents write each other's tagged session names; the user only knows the plain ones.
    let clean = |t: &str| if tag.is_empty() { t.to_string() } else { untag(t, tag) };
    let (who, color, text) = match entry {
        Entry::User(t) => ("You", Color32::from_rgb(120, 170, 230), t.as_str()),
        Entry::Incoming { from, text } => {
            ui.add_space(4.0);
            ui.colored_label(IDLE, egui::RichText::new(format!("From {}", clean(from))).strong());
            copy_menu(ui.label(clean(text)), clean(text));
            return;
        }
        Entry::Assistant(t) => (agent, Color32::from_rgb(140, 200, 140), t.as_str()),
        Entry::Tool(t) => {
            copy_menu(ui.weak(format!("  ⚙ {}", clean(t))), clean(t));
            return;
        }
        Entry::Error(t) => ("Error", FAILED, t.as_str()),
    };
    ui.add_space(4.0);
    ui.colored_label(color, egui::RichText::new(who).strong());
    copy_menu(ui.label(clean(text)), clean(text));
}

/// Right-click menu on a chat message: Copy for its whole text, Copy selection for the text selected in the log.
fn copy_menu(response: egui::Response, text: String) {
    response.context_menu(|ui| {
        if ui.button("Copy").clicked() {
            ui.ctx().copy_text(text);
        }
        let selected = ui.ctx().plugin::<egui::text_selection::LabelSelectionState>().lock().has_selection();
        if ui.add_enabled(selected, egui::Button::new("Copy selection")).clicked() {
            // Only egui can read the selected text, on a Copy event; `keep_label_selection` sends one next frame.
            ui.data_mut(|d| d.insert_temp(egui::Id::new(COPY_SELECTION), true));
        }
    });
}

const COPY_SELECTION: &str = "copy_label_selection";

/// Draws the chat log with its label text selection surviving a right-click. egui drops the selection on any press,
/// so while the right button is involved or a menu is open, the selection from the last quiet frame is put back.
fn keep_label_selection(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    use egui::{Event, PointerButton::Secondary, text_selection::LabelSelectionState};
    let (snapshot, copy) = (egui::Id::new("label_selection_snapshot"), egui::Id::new(COPY_SELECTION));
    let copying = ui.data_mut(|d| d.remove_temp::<bool>(copy)).unwrap_or(false);
    if copying {
        // Labels gather the selected text on a Copy event; egui copies it at the end of the pass, after any TextEdit.
        ui.input_mut(|i| i.events.push(Event::Copy));
    }
    let keep = ui.input(|i| i.pointer.button_down(Secondary) || i.pointer.button_released(Secondary)) || ui.ctx().any_popup_open();
    add(ui);
    let plugin = ui.ctx().plugin::<LabelSelectionState>();
    if keep {
        if let Some(saved) = ui.data(|d| d.get_temp::<LabelSelectionState>(snapshot)) {
            *plugin.lock() = saved;
        }
    } else {
        let now = plugin.lock().clone();
        ui.data_mut(|d| d.insert_temp(snapshot, now));
    }
}

#[cfg(test)]
mod tests {
    use super::{question_answers, slash_query};

    #[test]
    fn finds_the_slash_word_being_typed() {
        assert_eq!(slash_query("/"), Some((0, String::new())));
        assert_eq!(slash_query("/Sim"), Some((0, "sim".into())));
        assert_eq!(slash_query("please /rev"), Some((7, "rev".into())));
        assert_eq!(slash_query("line\n/x"), Some((5, "x".into())));
    }

    #[test]
    fn question_answers_join_ticked_labels_and_other_text() {
        let qs: Vec<serde_json::Value> = serde_json::from_str(r#"[
            {"question":"Color?","options":[{"label":"Red"},{"label":"Blue"}]},
            {"question":"Sizes?","multiSelect":true,"options":[{"label":"S"},{"label":"M"},{"label":"L"}]}]"#).unwrap();
        let picks = [(vec![false, true], String::new()), (vec![true, false, true], " XL ".into())];
        let a = question_answers(&qs, &picks);
        assert_eq!(a["Color?"], "Blue");
        assert_eq!(a["Sizes?"], "S, L, XL");
        assert_eq!(slash_query("a/b"), None);
        assert_eq!(slash_query("/simplify "), None);
        assert_eq!(slash_query(""), None);
    }
}

mod attach;
mod history;
mod links;
mod live;
mod markdown;
mod mcp;
mod prompt;
mod runner;
mod scene;
mod setup;
mod tasks;
mod team;
mod usage;

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
    Roles,
    Chat,
}

const MODELS: [&str; 4] = ["", "opus", "sonnet", "haiku"];
/// `claude --effort` levels; "" = no flag.
const EFFORTS: [&str; 6] = ["", "low", "medium", "high", "xhigh", "max"];

/// An agent state's colour, readable on the current theme's panels.
fn state_color(ui: &egui::Ui, state: NodeState) -> Color32 {
    let [r, g, b] = state.rgb(ui.visuals().dark_mode);
    Color32::from_rgb(r, g, b)
}

fn rgb([r, g, b]: [u8; 3]) -> Color32 {
    Color32::from_rgb(r, g, b)
}

/// egui's look for a theme: its dark or light base, recoloured with the theme's palette.
fn theme_visuals(theme: scene::Theme) -> egui::Visuals {
    let p = theme.palette();
    let mut v = if p.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.text_options.subpixel_binning = false;
    v.panel_fill = rgb(p.panel);
    v.window_fill = rgb(p.window);
    v.window_stroke.color = rgb(p.line);
    v.extreme_bg_color = rgb(p.field);
    v.faint_bg_color = rgb(p.faint);
    v.code_bg_color = rgb(p.faint);
    v.hyperlink_color = rgb(p.accent);
    v.weak_text_color = Some(rgb(p.weak));
    v.selection.bg_fill = rgb(p.panel).lerp_to_gamma(rgb(p.accent), if p.dark { 0.45 } else { 0.3 });
    v.selection.stroke.color = rgb(p.text);
    v.text_cursor.stroke.color = rgb(p.accent);
    let w = &mut v.widgets;
    for (state, fill) in [(&mut w.noninteractive, p.panel), (&mut w.inactive, p.widget), (&mut w.hovered, p.hover), (&mut w.active, p.hover), (&mut w.open, p.widget)] {
        state.bg_fill = rgb(fill);
        state.weak_bg_fill = rgb(fill);
        state.fg_stroke.color = rgb(p.body);
    }
    w.noninteractive.bg_stroke.color = rgb(p.line);
    w.hovered.fg_stroke.color = rgb(p.text);
    w.active.fg_stroke.color = rgb(p.text);
    v
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("session-start") {
        session_start();
        return;
    }
    usage::start();
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
        .add_systems(EguiPrimaryContextPass, (set_text_rendering, ui_system).chain())
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

/// Sharper text than egui's defaults, at the same size so the layout does not move: Ubuntu Regular instead of
/// the thin Ubuntu Light, whose stems came out as soft grey antialiasing, and glyphs snapped to whole pixels instead
/// of drawn at fractional offsets (sub-pixel binning), which blurred them further.
fn set_text_rendering(mut contexts: EguiContexts, mut done: Local<bool>) -> Result {
    if *done {
        return Ok(());
    }
    let ctx = contexts.ctx_mut()?;
    ctx.set_fonts(font_definitions());
    ctx.all_styles_mut(|style| style.visuals.text_options.subpixel_binning = false);
    *done = true;
    Ok(())
}

fn font_definitions() -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "Ubuntu-Regular".into(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!("../res/fonts/Ubuntu-Regular.ttf"))),
    );
    for family in fonts.families.values_mut() {
        for name in family.iter_mut().filter(|n| *n == "Ubuntu-Light") {
            *name = "Ubuntu-Regular".into();
        }
    }
    fonts.font_data.remove("Ubuntu-Light");
    // Ubuntu and the emoji fonts lack common symbols that the UI and agents' messages use (← → ● ✕ ✗ ✘), which
    // drew as empty boxes. DejaVu Sans has them with proportional spacing; Hack, already loaded for code, comes last
    // for anything DejaVu lacks (its arrows fill a whole monospace cell, so "← Back" read as "←Back").
    fonts.font_data.insert(
        "DejaVuSans".into(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!("../res/fonts/DejaVuSans.ttf"))),
    );
    fonts.families.entry(egui::FontFamily::Proportional).or_default().extend(["DejaVuSans".into(), "Hack".into()]);
    fonts
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
    /// For agents whose role wants a fresh session per task: the completed tasks they owned when their session
    /// started, by (team, agent). A completed task not in it ends the session.
    finished_seen: HashMap<(String, String), std::collections::HashSet<String>>,
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
            finished_seen: HashMap::new(),
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
            self.fresh_sessions();
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

    /// Restarts, on a new session under the same name, each agent whose role asks for a fresh session per task
    /// once a task it owns is completed and it has finished its turn. Its chat stays on screen.
    fn fresh_sessions(&mut self) {
        let mut restart = Vec::new();
        let mut lists: HashMap<String, Vec<tasks::Task>> = HashMap::new();
        for ((team_name, name), agent) in &self.running {
            let Some(team) = self.store.teams.iter().find(|t| t.name == *team_name) else { continue };
            let Some(member) = team.members.iter().find(|m| m.name == *name) else { continue };
            let key = (team_name.clone(), name.clone());
            if !team.role_of(member).is_some_and(|r| r.fresh_session) {
                self.finished_seen.remove(&key);
                continue;
            }
            let tag = self.tags.get(team_name).cloned().unwrap_or_default();
            let list = lists.entry(team_name.clone()).or_insert_with(|| tasks::read(&team.task_list_id()));
            let done: std::collections::HashSet<String> = list
                .iter()
                .filter(|t| t.done() && t.owner.as_deref().is_some_and(|o| o == name || untag(o, &tag) == *name))
                .map(|t| t.id.clone())
                .collect();
            let seen = self.finished_seen.entry(key.clone()).or_insert_with(|| done.clone());
            if done.is_subset(seen) || !agent.at_rest() {
                continue;
            }
            *seen = done;
            restart.push(key);
        }
        for (team_name, name) in restart {
            let Some(team) = self.store.teams.iter().find(|t| t.name == team_name) else { continue };
            let tag = self.tags.get(&team_name).cloned().unwrap_or_else(new_tag);
            let Some(old) = self.running.remove(&(team_name.clone(), name.clone())) else { continue };
            let mut earlier = old.entries();
            // Stop the old session first: two sessions must not share the name.
            drop(old);
            earlier.push(Entry::Tool("Task completed: new session for the next task (fresh session per task)".into()));
            if let Some(agent) = start_seat(team, &name, &tag, None) {
                agent.preload(earlier);
                self.running.insert((team_name, name), agent);
            }
        }
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
    mut styled: Local<Option<scene::Theme>>,
) -> Result {
    let ctx = contexts.ctx_mut()?.clone();
    let f = &mut *factory;
    if *styled != Some(f.store.theme) {
        *styled = Some(f.store.theme);
        let visuals = theme_visuals(f.store.theme);
        let mode = if visuals.dark_mode { egui::Theme::Dark } else { egui::Theme::Light };
        ctx.set_visuals_of(mode, visuals);
        ctx.set_theme(mode);
    }
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
                    ui.colored_label(state_color(ui, NodeState::Working), "Claude is set up: agent teams on, team-factory mod loaded");
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
            ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), usage_panel);
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
            // Laid out in a child that the panel does not size itself from: a row wider than the panel would
            // otherwise grow the panel's rect past its edge, so egui drew its resize line inside the content and the
            // scene got a viewport sized for that wider panel.
            let rect = ui.available_rect_before_wrap();
            let mut inner = ui.new_child(UiBuilder::new().id_salt("details").max_rect(rect));
            inner.set_clip_rect(rect.intersect(ui.clip_rect()));
            details_panel(&mut inner, f, &mut scene.messages);
            ui.allocate_rect(rect, egui::Sense::hover());
        })
        .response
        .rect
        .width();

    // The Bevy scene fills what the panels leave free. The panel widths are in egui points, so convert them with
    // egui's own scale: the window's times the zoom (Ctrl +/-). With the window's scale alone, a zoomed UI left the
    // scene click area stretched over the panels, where it swallowed every click.
    let scale = ctx.pixels_per_point();
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
        ("not started", NodeState::Off),
        ("starting", NodeState::Starting),
        ("working", NodeState::Working),
        ("idle", NodeState::Idle),
        ("stopped", NodeState::Failed),
    ];
    // Painted dots: the default egui font has no bullet glyph.
    egui::Area::new("legend".into())
        .fixed_pos(rect.left_bottom() + egui::vec2(14.0, -22.0 * items.len() as f32 - 8.0))
        .interactable(false)
        .show(ctx, |ui| {
            for (label, state) in items {
                let color = state_color(ui, state);
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
                            if ui.small_button("🗙").on_hover_text("Dismiss").clicked() {
                                dismiss = Some(i);
                            }
                        });
                    });
                    links::label(ui, text, None);
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
    egui::Frame::popup(ui.style()).stroke(egui::Stroke::new(1.5, state_color(ui, NodeState::Idle))).show(ui, |ui| {
        ui.strong(format!("{agent} asks"));
        for (q, (ticked, other)) in questions.iter().zip(picks.iter_mut()) {
            let options = q["options"].as_array().map(Vec::as_slice).unwrap_or_default();
            ticked.resize(options.len(), false);
            let multi = q["multiSelect"] == true;
            ui.separator();
            links::label(ui, q["question"].as_str().unwrap_or(""), None);
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
        ui.selectable_value(&mut f.view, View::Roles, egui::RichText::new("Roles").heading());
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
        View::Roles => {
            egui::ScrollArea::vertical().show(ui, |ui| roles_editor(ui, team));
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
        specs.push(NodeSpec { key: m.name.clone(), title: m.name.clone(), subtitle: subtitle(m.role_label(), status), state: st, is_lead: false, thumbnail: thumbnail(&m.name) });
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
                ("", state_color(ui, NodeState::Working))
            } else if task.done() {
                ("✔", state_color(ui, NodeState::Done))
            } else if blocked {
                ("⏸", ui.visuals().weak_text_color())
            } else {
                ("○", state_color(ui, NodeState::Idle))
            };
            let frame = egui::Frame::group(ui.style()).fill(if task.in_progress() {
                state_color(ui, NodeState::Working).gamma_multiply(0.09)
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

/// Folder picked by `pick_dir`, as (team name, path), waiting for that team's editor to take it.
static PICKED_DIR: std::sync::Mutex<Option<(String, String)>> = std::sync::Mutex::new(None);

fn take_picked_dir(team: &str) -> Option<String> {
    let mut picked = PICKED_DIR.lock().unwrap();
    if picked.as_ref().is_some_and(|(t, _)| t == team) { picked.take().map(|(_, dir)| dir) } else { None }
}

/// Opens a folder chooser on a thread so the UI keeps running; needs zenity or kdialog, like `pick_file`.
fn pick_dir(ctx: egui::Context, team: String, current: &str) {
    let start = if current.is_empty() { dirs::home_dir().unwrap_or_default().display().to_string() } else { current.to_string() };
    std::thread::spawn(move || {
        use std::process::Command;
        let zenity = Command::new("zenity").args(["--file-selection", "--directory", "--filename"]).arg(format!("{start}/")).output();
        let Ok(out) = zenity.or_else(|_| Command::new("kdialog").args(["--getexistingdirectory", &start]).output()) else {
            return;
        };
        let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if out.status.success() && !dir.is_empty() {
            *PICKED_DIR.lock().unwrap() = Some((team, dir));
            ctx.request_repaint();
        }
    });
}

/// A grid cell whose widgets wrap onto more lines when the panel is narrow. Straight in a grid, a wrapped row
/// overlaps the next one: the vertical scope makes the grid see its full height.
fn wrapped_cell(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ui.vertical(|ui| ui.horizontal_wrapped(add));
}

fn team_editor(ui: &mut egui::Ui, team: &mut Team, delete: &mut bool, export: &mut bool) {
    egui::Grid::new("team").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Name");
        wrapped_cell(ui, |ui| {
            ui.add(egui::TextEdit::singleline(&mut team.name).desired_width(160.0));
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
        ui.horizontal(|ui| {
            if let Some(dir) = take_picked_dir(&team.name) {
                team.directory = dir;
            }
            let browse = ui.button("Browse…").on_hover_text("Choose the folder");
            ui.add(
                egui::TextEdit::singleline(&mut team.directory)
                    .hint_text("Only sessions started in this folder get the team (empty = every session)")
                    .desired_width(f32::INFINITY),
            );
            if browse.clicked() {
                pick_dir(ui.ctx().clone(), team.name.clone(), &team.directory);
            }
        });
        ui.end_row();
        ui.label("Lead model");
        wrapped_cell(ui, |ui| {
            let shown = if team.lead_model.is_empty() { "default" } else { team.lead_model.as_str() };
            egui::ComboBox::from_id_salt("lead_model").selected_text(shown).show_ui(ui, |ui| {
                for model in MODELS {
                    ui.selectable_value(&mut team.lead_model, model.to_string(), if model.is_empty() { "default" } else { model });
                }
            });
            ui.label("Effort");
            pick(ui, "lead_effort", &mut team.lead_effort, &EFFORTS, "default");
            ui.label(egui::RichText::new("Also used by agents and roles left on \"lead's\"").weak());
        });
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
    let team_roles = team.roles.clone();
    for (i, m) in team.members.iter_mut().enumerate() {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            // Wraps onto a second line when the panel is narrow.
            ui.horizontal_wrapped(|ui| {
                ui.label("Name");
                ui.add(egui::TextEdit::singleline(&mut m.name).desired_width(140.0));
                ui.label("Role");
                egui::ComboBox::from_id_salt(("role", i)).selected_text(m.role_label().to_string()).show_ui(ui, |ui| {
                    for r in Role::ALL {
                        if ui.selectable_label(m.team_role.is_empty() && m.role == r, r.label()).clicked() {
                            m.role = r;
                            m.team_role.clear();
                        }
                    }
                    // Team roles named like a built-in one already apply through it.
                    for r in team_roles.iter().filter(|r| !r.name.trim().is_empty() && !Role::ALL.iter().any(|b| b.label() == r.name.trim())) {
                        if ui.selectable_label(m.team_role == r.name.trim(), r.name.trim()).clicked() {
                            m.role = Role::Custom;
                            m.team_role = r.name.trim().to_string();
                        }
                    }
                });
                let was = m.override_prompt;
                ui.checkbox(&mut m.override_prompt, "Override role prompt")
                    .on_hover_text("On: this agent uses its own prompt below. Off: it uses its role's prompt from the Roles tab, and follows its edits.");
                // Start the custom prompt from the role's, unless the agent already has one of its own.
                if m.override_prompt && !was && m.instructions.trim().is_empty() {
                    m.override_prompt = false;
                    m.instructions = m.prompt(&team_roles).to_string();
                    m.override_prompt = true;
                }
                ui.label("Model");
                pick(ui, ("model", i), &mut m.model, &MODELS, "role's / lead's");
                ui.label("Effort");
                pick(ui, ("effort", i), &mut m.effort, &EFFORTS, "role's / lead's");
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
            if m.override_prompt {
                ui.add(
                    egui::TextEdit::multiline(&mut m.instructions)
                        .hint_text("What this agent does and must not do")
                        .desired_rows(2)
                        .desired_width(f32::INFINITY),
                );
            } else {
                let mut shown = m.prompt(&team_roles).to_string();
                ui.add_enabled(false, egui::TextEdit::multiline(&mut shown).hint_text("This role has no prompt").desired_rows(2).desired_width(f32::INFINITY));
            }
        });
    }
    if let Some(i) = swap {
        team.members.swap(i, i + 1);
    }
    if let Some(i) = remove {
        team.members.remove(i);
    }
    ui.horizontal_wrapped(|ui| {
        for role in Role::ALL {
            if ui.button(format!("+ {}", role.label())).clicked() {
                add_member(team, Member::new(&format!("{} {}", role.label(), team.members.len() + 1), role));
            }
        }
        for r in team_roles.iter().filter(|r| !r.name.trim().is_empty() && !Role::ALL.iter().any(|b| b.label() == r.name.trim())) {
            if ui.button(format!("+ {}", r.name.trim())).clicked() {
                let mut m = Member::new(&format!("{} {}", r.name.trim(), team.members.len() + 1), Role::Custom);
                m.team_role = r.name.trim().to_string();
                add_member(team, m);
            }
        }
    });
}

/// A dropdown over `options`, where "" is shown as `unset`.
fn pick(ui: &mut egui::Ui, id: impl egui::AsIdSalt, value: &mut String, options: &[&str], unset: &str) {
    let shown = if value.is_empty() { unset } else { value.as_str() }.to_string();
    egui::ComboBox::from_id_salt(id).selected_text(shown).show_ui(ui, |ui| {
        for o in options {
            ui.selectable_value(value, o.to_string(), if o.is_empty() { unset } else { o });
        }
    });
}

fn roles_editor(ui: &mut egui::Ui, team: &mut Team) {
    let servers = mcp::available_cached(&team.working_dir());
    ui.heading("Roles");
    ui.weak("Default prompts for this team's agents. An agent uses its role's prompt, model and effort here, and follows edits to them, unless \"Override role prompt\" is ticked on it in Design. A role named Developer, QA or Reviewer replaces that built-in role's prompt.");
    let mut remove = None;
    let mut renamed = None;
    for (i, r) in team.roles.iter_mut().enumerate() {
        let before = r.name.trim().to_string();
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Name");
                ui.add(egui::TextEdit::singleline(&mut r.name).desired_width(180.0));
                if r.name.trim() != before {
                    renamed = Some((before.clone(), r.name.trim().to_string()));
                }
                ui.label("Model");
                pick(ui, ("role_model", i), &mut r.model, &MODELS, "lead's");
                ui.label("Effort");
                pick(ui, ("role_effort", i), &mut r.effort, &EFFORTS, "lead's");
                ui.checkbox(&mut r.fresh_session, "Fresh session per task").on_hover_text(
                    "When a task an agent in this role owns is completed and the agent is idle, restart it on a new session \
                     (same name, same prompt) so the next task doesn't pay for the old context.",
                );
                if ui.button("Delete").clicked() {
                    remove = Some(i);
                }
            });
            ui.add(
                egui::TextEdit::multiline(&mut r.prompt)
                    .hint_text("The default prompt: what an agent in this role does and must not do")
                    .desired_rows(4)
                    .desired_width(f32::INFINITY),
            );
            mcp_picker(ui, i, r, &servers);
        });
    }
    // Agents follow their role across a rename.
    if let Some((old, new)) = renamed {
        for m in team.members.iter_mut().filter(|m| !old.is_empty() && m.team_role == old) {
            m.team_role = new.clone();
        }
    }
    if let Some(i) = remove {
        team.roles.remove(i);
    }
    ui.horizontal(|ui| {
        if ui.button("+ Role").clicked() {
            team.roles.push(team::TeamRole::new(&format!("Role {}", team.roles.len() + 1), ""));
        }
        // Starting points: copies of the built-in roles, skipped once a role of that name exists.
        for builtin in Role::ALL.into_iter().filter(|r| *r != Role::Custom) {
            if !team.roles.iter().any(|r| r.name == builtin.label())
                && ui.button(format!("+ {}", builtin.label())).on_hover_text("Copy the built-in role").clicked()
            {
                team.roles.push(team::TeamRole::new(builtin.label(), builtin.default_instructions()));
            }
        }
    });
}

/// "All" or a checklist of the servers the role's agents load. Takes effect when an agent next starts.
fn mcp_picker(ui: &mut egui::Ui, i: usize, role: &mut team::TeamRole, servers: &[mcp::Server]) {
    let mut all = role.mcp_servers.is_none();
    ui.horizontal(|ui| {
        ui.label("MCP servers");
        if ui.checkbox(&mut all, "All").on_hover_text("Load every MCP server Claude Code would, as a normal session does").changed() {
            // Unticking starts from the full list, so the user only unticks what the role doesn't need.
            role.mcp_servers = if all { None } else { Some(servers.iter().map(|s| s.name.clone()).collect()) };
        }
    });
    let Some(selected) = role.mcp_servers.as_mut() else { return };
    let n = selected.iter().filter(|n| servers.iter().any(|s| s.name == **n)).count();
    egui::CollapsingHeader::new(format!("{n} of {} servers selected", servers.len())).id_salt(("role_mcp", i)).show(ui, |ui| {
        ui.weak("Applies when an agent next starts. Picking a claude.ai connector starts every server and hides the tools of the unpicked ones; otherwise unpicked servers don't start at all.");
        ui.horizontal_wrapped(|ui| {
            for s in servers {
                let mut on = selected.contains(&s.name);
                if ui.checkbox(&mut on, &s.name).on_hover_text(format!("{} server", s.source)).changed() {
                    if on { selected.push(s.name.clone()) } else { selected.retain(|n| *n != s.name) }
                }
            }
        });
    });
}

fn routines_editor(ui: &mut egui::Ui, team: &mut Team) {
    use team::{Routine, Schedule};
    ui.heading("Routines");
    ui.weak("Prompts sent to the lead on a schedule, while the app and the lead are running. Missed runs are skipped.");
    let mut remove = None;
    for (i, r) in team.routines.iter_mut().enumerate() {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label("Name");
                ui.add(egui::TextEdit::singleline(&mut r.name).desired_width(140.0));
                ui.checkbox(&mut r.enabled, "Enabled");
                ui.weak(if r.last_run == 0 { "never run".to_string() } else { format!("last run {}", history::format_time(r.last_run)) });
                if ui.button("Remove").clicked() {
                    remove = Some(i);
                }
            });
            ui.horizontal_wrapped(|ui| {
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
    // Wrapped to the panel: a long line no longer runs under the Tasks panel.
    ui.add(egui::TextEdit::multiline(&mut text).code_editor().interactive(false).desired_width(ui.available_width()));
}

/// The chat panel's share of the app state.
struct Chat<'a> {
    running: &'a mut Running,
    tags: &'a mut HashMap<String, String>,
    chat_with: &'a mut String,
    draft: &'a mut String,
    attachments: &'a mut Vec<std::path::PathBuf>,
}

/// Tallest the chat input grows before it scrolls inside.
const CHAT_INPUT_MAX_HEIGHT: f32 = 160.0;

/// "1.2M tok · $3.45" for a usage, with the token breakdown kept for the hover.
fn usage_text(u: runner::Usage) -> String {
    fn short(n: u64) -> String {
        match n {
            0..1_000 => n.to_string(),
            1_000..1_000_000 => format!("{:.1}k", n as f64 / 1e3),
            _ => format!("{:.1}M", n as f64 / 1e6),
        }
    }
    format!("{} tok · ${:.2}", short(u.tokens()), u.cost_usd)
}

fn usage_detail(u: runner::Usage) -> String {
    format!("input {}, output {}, cache read {}, cache write {}", u.input, u.output, u.cache_read, u.cache_write)
}

/// The selected agent's tokens and cost, and the team's total with a per-agent breakdown on hover.
fn usage_line(ui: &mut egui::Ui, team: &Team, running: &Running, chat_with: &str) {
    let lead = team.lead_name();
    let names = std::iter::once(lead.clone()).chain(team.members.iter().map(|m| m.name.clone()));
    let per_agent: Vec<(String, runner::Usage)> =
        names.filter_map(|n| running.get(&(team.name.clone(), n.clone())).map(|a| (n, a.usage()))).collect();
    let mut total = runner::Usage::default();
    per_agent.iter().for_each(|(_, u)| total += *u);
    ui.horizontal(|ui| {
        if let Some((_, u)) = per_agent.iter().find(|(n, _)| n == chat_with) {
            let who = if chat_with == lead { "Lead" } else { chat_with };
            ui.label(egui::RichText::new(format!("{who}: {}", usage_text(*u))).weak()).on_hover_text(usage_detail(*u));
            ui.separator();
        }
        let breakdown: Vec<String> = per_agent
            .iter()
            .map(|(n, u)| format!("{}: {}", if *n == lead { "Lead" } else { n }, usage_text(*u)))
            .collect();
        ui.label(egui::RichText::new(format!("Team: {}", usage_text(total))).weak())
            .on_hover_text(format!("{}\n\n{}\n\nCounted from each session's start, resumed sessions included; updated after every turn.", breakdown.join("\n"), usage_detail(total)));
    });
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
    if team_running {
        usage_line(ui, team, running, chat_with);
    }
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

    // Declared before the log so the log takes what is left; the input grows with its text up to a limit, then scrolls.
    egui::Panel::bottom("chat_input_panel").frame(egui::Frame::NONE).show(ui, |ui| {
        ui.separator();
        let temp = std::env::temp_dir();
        let mut removed = None;
        ui.horizontal_wrapped(|ui| {
            for (i, path) in attachments.iter().enumerate() {
                let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
                ui.label(format!("📎 {name}")).on_hover_text(path.display().to_string());
                if ui.small_button("🗙").on_hover_text("Remove this attachment").clicked() {
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
                .desired_width(f32::INFINITY)
                .return_key(KeyboardShortcut::new(Modifiers::SHIFT, Key::Enter));
            // The scroll area keeps the caret in view once the text is taller than the limit.
            let response = egui::ScrollArea::vertical()
                .id_salt("chat_input_scroll")
                .max_height(CHAT_INPUT_MAX_HEIGHT)
                .max_width(ui.available_width() - 70.0)
                .auto_shrink([false, true])
                .show(ui, |ui| ui.add(edit))
                .inner;
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
    });

    egui::ScrollArea::vertical()
        .id_salt(("chat", &key.1))
        .stick_to_bottom(true)
        .auto_shrink(false)
        .show(ui, |ui| {
            brighten_chat_text(ui);
            keep_label_selection(ui, |ui| {
                for entry in agent.entries() {
                    chat_entry(ui, &key.1, &entry, tags.get(&team.name).map_or("", |t| t.as_str()));
                }
            });
            if agent.status() == Status::Working {
                ui.weak(format!("{} is working…", key.1));
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
    // The lead runs on the team's lead model; each member on its own pick, or the lead's when it has none.
    let cwd = team.working_dir();
    let (model, effort, system_prompt, mcp_args) = if name == team.lead_name() {
        (team.lead_model.clone(), team.lead_effort.clone(), prompt::render_lead(team, tag), Vec::new())
    } else {
        let m = team.members.iter().find(|m| m.name == name)?;
        let mcp_args = team.role_of(m).and_then(|r| r.mcp_servers.as_ref()).map_or_else(Vec::new, |sel| mcp::launch_args(&mcp::available(&cwd), sel));
        (team.model_of(m), team.effort_of(m), prompt::render_member(team, m, tag), mcp_args)
    };
    let session_name = prompt::tagged(name.trim(), tag);
    let launch = runner::Launch {
        name: &session_name,
        cwd: &cwd,
        model: &model,
        effort: &effort,
        permission_mode: &team.permission_mode,
        system_prompt,
        task_list_id: &team.task_list_id(),
        resume,
        mcp_args,
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
            brighten_chat_text(ui);
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

/// Gives a long chat the theme's strongest text, and weak text halfway to it, for this ui and its children only.
fn brighten_chat_text(ui: &mut egui::Ui) {
    let visuals = &mut ui.style_mut().visuals;
    let strong = visuals.strong_text_color();
    visuals.weak_text_color = Some(visuals.weak_text_color().lerp_to_gamma(strong, 0.3));
    visuals.widgets.noninteractive.fg_stroke.color = strong;
}

/// The theme's accent: the "You" label, and faintly behind the user's own messages.
fn you_color(ui: &egui::Ui) -> Color32 {
    ui.visuals().hyperlink_color
}

fn chat_entry(ui: &mut egui::Ui, agent: &str, entry: &Entry, tag: &str) {
    if let Entry::User(text) = entry {
        let clean = if tag.is_empty() { text.clone() } else { untag(text, tag) };
        ui.add_space(4.0);
        egui::Frame::new().fill(you_color(ui).gamma_multiply(0.12)).corner_radius(4.0).inner_margin(6.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.colored_label(you_color(ui), egui::RichText::new("You").strong());
            copy_menu(markdown::show(ui, &clean), clean.clone());
        });
        return;
    }
    // Agents write each other's tagged session names; the user only knows the plain ones.
    let clean = |t: &str| if tag.is_empty() { t.to_string() } else { untag(t, tag) };
    let (who, color, text) = match entry {
        Entry::User(t) => ("You", you_color(ui), t.as_str()),
        Entry::Incoming { from, text } => {
            ui.add_space(4.0);
            ui.colored_label(state_color(ui, NodeState::Idle), egui::RichText::new(format!("From {}", clean(from))).strong());
            copy_menu(markdown::show(ui, &clean(text)), clean(text));
            return;
        }
        Entry::Assistant(t) => (agent, state_color(ui, NodeState::Working), t.as_str()),
        Entry::Tool(t) => {
            let weak = ui.visuals().weak_text_color();
            copy_menu(links::label(ui, &format!("  ⚙ {}", clean(t)), Some(weak)), clean(t));
            return;
        }
        Entry::Error(t) => ("Error", state_color(ui, NodeState::Failed), t.as_str()),
    };
    ui.add_space(4.0);
    ui.colored_label(color, egui::RichText::new(who).strong());
    copy_menu(markdown::show(ui, &clean(text)), clean(text));
}

/// Right-click menu on a chat message: Copy for its whole text, Copy selection for the text selected in the log.
/// Opens on a right-click anywhere over `response`, even one that only senses hover (a markdown message).
fn copy_menu(response: egui::Response, text: String) {
    let right_clicked = response.contains_pointer() && response.ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Secondary));
    let open = right_clicked.then_some(egui::SetOpenCommand::Bool(true));
    egui::Popup::menu(&response).open_memory(open).at_pointer_fixed().show(|ui| {
        if ui.button("Copy").clicked() {
            ui.ctx().copy_text(text);
        }
        let selected = all_selected(ui.ctx()) || ui.ctx().plugin::<egui::text_selection::LabelSelectionState>().lock().has_selection();
        if ui.add_enabled(selected, egui::Button::new("Copy selection")).clicked() {
            // Only egui can read the selected text, on a Copy event; `keep_label_selection` sends one next frame.
            ui.data_mut(|d| d.insert_temp(egui::Id::new(COPY_SELECTION), true));
        }
    });
}

const COPY_SELECTION: &str = "copy_label_selection";

/// Draws the chat log with its label text selection surviving a right-click. egui drops the selection on any press,
/// so while the right button is involved or a menu is open, the selection from the last quiet frame is put back.
/// Also: Ctrl+A selects the whole log, off-screen text included, until the next click or Esc; and a drag that
/// leaves the log scrolls it, so the selection can grow past the edge.
fn keep_label_selection(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    use egui::{Event, PointerButton::Secondary, text_selection::LabelSelectionState};
    let (snapshot, copy) = (egui::Id::new("label_selection_snapshot"), egui::Id::new(COPY_SELECTION));
    let copying = ui.data_mut(|d| d.remove_temp::<bool>(copy)).unwrap_or(false);
    if copying {
        // Labels gather the selected text on a Copy event; egui copies it at the end of the pass, after any TextEdit.
        ui.input_mut(|i| i.events.push(Event::Copy));
    }
    let keep = ui.input(|i| i.pointer.button_down(Secondary) || i.pointer.button_released(Secondary)) || egui::Popup::is_any_open(ui.ctx());
    // egui clears the selection at the end of the pass on a press that is not on a label, such as one on the menu's Copy selection.
    LabelSelectionState::keep_selection(ui.ctx(), keep);
    let all = select_all_state(ui, keep);
    LabelSelectionState::select_all(ui.ctx(), all);
    add(ui);
    LabelSelectionState::select_all(ui.ctx(), false);
    scroll_while_selecting(ui);
    let plugin = ui.ctx().plugin::<LabelSelectionState>();
    if keep {
        if let Some(saved) = ui.data(|d| d.get_temp::<LabelSelectionState>(snapshot)) {
            *plugin.lock() = saved;
        }
        // The snapshot has the flag off; egui looks at it at the end of the pass.
        LabelSelectionState::keep_selection(ui.ctx(), true);
    } else {
        let now = plugin.lock().clone();
        ui.data_mut(|d| d.insert_temp(snapshot, now));
    }
}

const SELECT_ALL: &str = "chat_select_all";

/// Whether the whole log is selected: Ctrl+A turns it on (unless a text field has the keyboard, where it selects
/// the field's text), a press or Esc turns it off (`keep`: the right-click menu is in use, leave it).
fn select_all_state(ui: &mut egui::Ui, keep: bool) -> bool {
    let id = egui::Id::new(SELECT_ALL);
    let mut on = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    // Not for the right-click menu, which works on the selection.
    if !keep && ui.input(|i| i.pointer.any_pressed() || i.key_pressed(Key::Escape)) {
        on = false;
    }
    if !ui.ctx().egui_wants_keyboard_input() && ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::A)) {
        ui.ctx().plugin::<egui::text_selection::LabelSelectionState>().lock().clear_selection();
        on = true;
    }
    ui.data_mut(|d| d.insert_temp(id, on));
    on
}

/// Whether the log is selected as a whole, for the right-click menu.
fn all_selected(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(egui::Id::new(SELECT_ALL))).unwrap_or(false)
}

/// While a drag that began in the log is held above or below it, scrolls the log, faster the further out the pointer
/// is. The selection follows, as it is laid out under the pointer. Call at the end of the scroll area's content.
fn scroll_while_selecting(ui: &egui::Ui) {
    let id = egui::Id::new("chat_drag_from_log");
    let view = ui.clip_rect();
    let (pressed, down, pos, dt) = ui.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.interact_pos(), i.stable_dt));
    if pressed {
        ui.data_mut(|d| d.insert_temp(id, pos.is_some_and(|p| view.contains(p))));
    }
    let from_log = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    let Some(pos) = pos.filter(|_| down && from_log) else { return };
    let outside = if pos.y < view.top() {
        view.top() - pos.y
    } else if pos.y > view.bottom() {
        view.bottom() - pos.y
    } else {
        return;
    };
    // Points per second: 300 right at the edge, more as the pointer moves away, capped.
    let speed = 300.0 + outside.abs().min(200.0) * 8.0;
    ui.scroll_with_delta(egui::vec2(0.0, speed * dt * outside.signum()));
    ui.ctx().request_repaint();
}

#[cfg(test)]
mod tests {
    use super::{font_definitions, question_answers, slash_query};
    use bevy_egui::egui;

    /// Every non-ASCII character in the string literals of the files that build UI text, skipping comment lines.
    fn ui_symbols() -> Vec<char> {
        let sources = [include_str!("main.rs"), include_str!("runner.rs"), include_str!("history.rs"), include_str!("markdown.rs")];
        let mut symbols: Vec<char> = sources
            .iter()
            .flat_map(|s| s.lines())
            .filter(|l| !l.trim_start().starts_with("//"))
            .flat_map(|l| l.split('"').skip(1).step_by(2))
            .flat_map(str::chars)
            .filter(|c| !c.is_ascii())
            .collect();
        symbols.sort_unstable();
        symbols.dedup();
        symbols
    }

    #[test]
    fn the_ui_font_has_every_symbol_the_ui_and_agents_use() {
        let symbols = ui_symbols();
        // Guards the scan itself: these are known to be in the UI.
        assert!(['←', '●', '🗙', '…'].iter().all(|c| symbols.contains(c)), "{symbols:?}");
        use skrifa::MetadataProvider;
        let defs = font_definitions();
        let faces: Vec<skrifa::FontRef> = defs.families[&egui::FontFamily::Proportional]
            .iter()
            .map(|name| skrifa::FontRef::new(defs.font_data[name].font.as_ref()).expect("a valid font"))
            .collect();
        // Symbols agents often write, beyond those in the UI's own strings.
        let in_messages = "✓✔✗✘✕✖❌✅⚠★→←↑↓↔⇒≥≤≠≈±×•●○▶◀─│┌┐└┘├┤┼…–—“”°™©®€π√∞";
        let missing: String = symbols
            .into_iter()
            .chain(in_messages.chars())
            .filter(|c| faces.iter().all(|f| f.charmap().map(*c).is_none()))
            .collect();
        assert!(missing.is_empty(), "drawn as empty boxes: {missing}");
    }

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

/// "Your usage limits" at the bottom of the teams panel. Drawn bottom-up, so rows go in reverse.
fn usage_panel(ui: &mut egui::Ui) {
    let Some(usage) = usage::latest() else { return };
    let Some(usage) = usage else {
        ui.weak("Usage unavailable");
        ui.separator();
        return;
    };
    let mut rows: Vec<(&str, String, f32)> = Vec::new();
    for (name, w) in [("5-hour limit", &usage.five_hour), ("Weekly \u{b7} all models", &usage.seven_day)] {
        let Some(w) = w else { continue };
        let pct = w.utilization.unwrap_or(0.0);
        let reset = w.resets_at.as_deref().and_then(usage::reset_label);
        let detail = match reset {
            Some(r) => format!("Resets {r} \u{b7} {pct:.0}%"),
            None => format!("{pct:.0}%"),
        };
        rows.push((name, detail, (pct / 100.0) as f32));
    }
    if let Some(e) = usage.extra_usage.as_ref().filter(|e| e.is_enabled)
        && let Some(limit) = e.monthly_limit.filter(|l| *l > 0.0)
    {
        let used = e.used_credits.unwrap_or(0.0);
        rows.push(("Usage credits", format!("{} of {}", dollars(used / 100.0), dollars(limit / 100.0)), (used / limit) as f32));
    }
    let blue = ui.visuals().hyperlink_color;
    ui.add_space(4.0);
    for (name, detail, frac) in rows.iter().rev() {
        ui.add_space(4.0);
        ui.add(egui::ProgressBar::new(frac.clamp(0.0, 1.0)).desired_height(3.0).fill(blue).corner_radius(1.5));
        ui.weak(detail);
        ui.label(*name);
    }
    ui.strong("Your usage limits");
    ui.separator();
}

/// `$1,000.00`.
fn dollars(v: f64) -> String {
    let cents = (v * 100.0).round() as i64;
    let whole = (cents / 100).to_string();
    let mut grouped = String::new();
    for (i, c) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    format!("${grouped}.{:02}", cents % 100)
}

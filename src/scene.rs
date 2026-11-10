//! The animated team view: a Bevy 2D scene drawn in the window area the egui panels leave free.

use bevy::input::mouse::{AccumulatedMouseScroll, MouseScrollUnit};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy::asset::RenderAssetUsages;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use std::f32::consts::TAU;
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NodeState {
    Off,
    Starting,
    Working,
    Idle,
    Done,
    Failed,
}

impl NodeState {
    fn color(self) -> LinearRgba {
        let c = match self {
            NodeState::Off => Srgba::rgb(0.32, 0.34, 0.40),
            NodeState::Starting => Srgba::rgb(0.40, 0.62, 0.95),
            NodeState::Working => Srgba::rgb(0.30, 0.82, 0.48),
            NodeState::Idle => Srgba::rgb(0.95, 0.68, 0.22),
            NodeState::Done => Srgba::rgb(0.38, 0.52, 0.92),
            NodeState::Failed => Srgba::rgb(0.92, 0.32, 0.32),
        };
        c.into()
    }
}

pub struct NodeSpec {
    /// The agent's name: what chats, clicks and messages refer to.
    pub key: String,
    pub title: String,
    pub subtitle: String,
    pub state: NodeState,
    pub is_lead: bool,
    /// The agent's nested desktop, drawn under its labels while it has one.
    pub thumbnail: Option<Arc<crate::runner::Thumbnail>>,
}

/// What the egui side wants drawn this frame.
#[derive(Resource, Default)]
pub struct SceneInput {
    pub nodes: Vec<NodeSpec>,
    pub selected: String,
    /// Messages to animate, as (from, to) node keys; `from` is `YOU` for what the user typed.
    pub messages: Vec<(String, String)>,
}

/// What the scene reports back to the egui side.
#[derive(Resource, Default)]
pub struct SceneOutput {
    pub clicked: Option<String>,
    /// Set on a right press in the scene: the key of the node under the cursor, or None on empty canvas.
    pub right_clicked: Option<Option<String>>,
}

pub const YOU: &str = "\u{0}you";

#[derive(Component)]
pub struct SceneCamera;

#[derive(Component)]
struct Node {
    key: String,
    is_lead: bool,
    state: NodeState,
    target: Vec2,
    color: LinearRgba,
    appear: f32,
    hover: f32,
    core: Handle<ColorMaterial>,
    halo: Handle<ColorMaterial>,
}

#[derive(Component)]
struct Halo;

/// The thumbnail sprite under a node, and the snapshot it currently shows.
#[derive(Component, Default)]
struct Preview(Option<Arc<crate::runner::Thumbnail>>);

#[derive(Component)]
struct Core;

#[derive(Component)]
enum Label {
    Initials,
    Title,
    Subtitle,
}

struct Orb {
    from: String,
    to: String,
    t: f32,
    color: LinearRgba,
}

struct Spark {
    pos: Vec2,
    age: f32,
    color: LinearRgba,
}

struct Ripple {
    pos: Vec2,
    age: f32,
    color: LinearRgba,
}

#[derive(Resource, Default)]
struct Effects {
    orbs: Vec<Orb>,
    sparks: Vec<Spark>,
    ripples: Vec<Ripple>,
    stars: Vec<(Vec2, f32)>,
}

const RADIUS: f32 = 34.0;
const LEAD_RADIUS: f32 = 42.0;

pub struct ScenePlugin;

impl Plugin for ScenePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SceneInput>()
            .init_resource::<SceneOutput>()
            .init_resource::<Effects>()
            .insert_resource(Zoom(1.0))
            .insert_resource(UserView { zoom: 1.0, pan: Vec2::ZERO, press: None, last: Vec2::ZERO, dragging: false })
            .insert_resource(ClearColor(Color::srgb(0.035, 0.04, 0.07)))
            .insert_gizmo_config(
                DefaultGizmoConfigGroup,
                GizmoConfig { line: GizmoLineConfig { width: 2.0, joints: GizmoLineJoint::Miter, ..default() }, ..default() },
            )
            .add_systems(Update, (pointer, fit_camera, sync_nodes, animate_nodes, spawn_orbs, animate_effects, draw).chain());
    }
}

/// The scene's size in world units: the viewport, grown by the camera's zoom-out.
fn view_size(camera: &Camera, zoom: &Zoom) -> Vec2 {
    camera.logical_viewport_size().unwrap_or(Vec2::new(800.0, 600.0)) * zoom.0
}

/// How far the camera is zoomed out so the whole chart fits the area the panels leave.
#[derive(Resource)]
struct Zoom(f32);

/// The user's own zoom (on top of the fit) and pan, and the left-button press in progress on the scene.
#[derive(Resource)]
struct UserView {
    zoom: f32,
    pan: Vec2,
    press: Option<Vec2>,
    last: Vec2,
    dragging: bool,
}

/// Minimum horizontal room per agent in the row, so names and statuses never overlap.
const SLOT: f32 = 210.0;

fn fit_camera(
    input: Res<SceneInput>,
    time: Res<Time>,
    mut zoom: ResMut<Zoom>,
    view: Res<UserView>,
    camera: Single<(&Camera, &mut Transform, &mut Projection), With<SceneCamera>>,
) {
    let (camera, mut transform, mut projection) = camera.into_inner();
    transform.translation = view.pan.extend(transform.translation.z);
    let Some(size) = camera.logical_viewport_size() else { return };
    let members = input.nodes.iter().filter(|n| !n.is_lead).count() as f32;
    let wanted_width = (members - 1.0).max(0.0) * SLOT + 2.0 * 90.0;
    let wanted_height = 480.0;
    let target = (wanted_width / size.x).max(wanted_height / size.y).max(1.0);
    zoom.0 += (target - zoom.0) * (1.0 - (-time.delta_secs() * 5.0).exp());
    if let Projection::Orthographic(ortho) = &mut *projection {
        ortho.scale = zoom.0 * view.zoom;
    }
}

/// Lead near the top, teammates in a row below it, both scaled to the free area.
fn layout(is_lead: bool, index: usize, members: usize, size: Vec2) -> Vec2 {
    // The chart (lead, drop, bus, member row and labels) is centred vertically in the free area.
    let depth = (size.y * 0.4).clamp(170.0, 300.0);
    let lead_y = depth / 2.0 + 20.0;
    if is_lead {
        return Vec2::new(0.0, lead_y);
    }
    // Evenly spaced like an org chart, but never spread wider than a comfortable gap per agent.
    let span = (size.x - 2.0 * 90.0).max(0.0).min((members.max(1) - 1) as f32 * 260.0);
    let x = if members <= 1 { 0.0 } else { -span / 2.0 + span * index as f32 / (members - 1) as f32 };
    Vec2::new(x, lead_y - depth)
}

fn sync_nodes(
    mut commands: Commands,
    input: Res<SceneInput>,
    camera: Single<&Camera, With<SceneCamera>>,
    mut nodes: Query<(Entity, &mut Node, &Children)>,
    mut labels: Query<(&Label, &mut Text2d)>,
    mut previews: Query<(&mut Preview, &mut Sprite, &mut Visibility, &mut Transform)>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    mut effects: ResMut<Effects>,
    zoom: Res<Zoom>,
) {
    let size = view_size(&camera, &zoom);
    if effects.stars.is_empty() {
        // Fixed pseudo-random stars: an LCG is enough and avoids a dependency.
        let mut seed = 0x2545_F491_u32;
        let mut next = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 24) as f32
        };
        effects.stars = (0..140).map(|_| (Vec2::new(next() - 0.5, next() - 0.5), next() * TAU)).collect();
    }

    for (entity, node, _) in &nodes {
        if !input.nodes.iter().any(|n| n.key == node.key) {
            commands.entity(entity).despawn();
        }
    }

    let members = input.nodes.iter().filter(|n| !n.is_lead).count();
    let mut member_index = 0;
    for spec in &input.nodes {
        let target = layout(spec.is_lead, member_index, members, size);
        if !spec.is_lead {
            member_index += 1;
        }
        if let Some((_, mut node, children)) = nodes.iter_mut().find(|(_, n, _)| n.key == spec.key) {
            node.target = target;
            node.state = spec.state;
            for child in children.iter() {
                if let Ok((label, mut text)) = labels.get_mut(child) {
                    let wanted = match label {
                        Label::Initials => initials(&spec.title),
                        Label::Title => spec.title.clone(),
                        Label::Subtitle => spec.subtitle.clone(),
                    };
                    if text.0 != wanted {
                        text.0 = wanted;
                    }
                }
                if let Ok((mut preview, mut sprite, mut visibility, mut transform)) = previews.get_mut(child) {
                    let same = match (&preview.0, &spec.thumbnail) {
                        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                        (a, b) => a.is_none() && b.is_none(),
                    };
                    if same {
                        continue;
                    }
                    preview.0 = spec.thumbnail.clone();
                    let Some(t) = &spec.thumbnail else {
                        *visibility = Visibility::Hidden;
                        continue;
                    };
                    let size = Extent3d { width: t.width, height: t.height, depth_or_array_layers: 1 };
                    let image =
                        Image::new(size, TextureDimension::D2, t.rgba.clone(), TextureFormat::Rgba8UnormSrgb, RenderAssetUsages::default());
                    // The old snapshot is freed when its last handle goes.
                    sprite.image = images.add(image);
                    let radius = if spec.is_lead { LEAD_RADIUS } else { RADIUS };
                    transform.translation.y = -radius - 54.0 - t.height as f32 / 2.0;
                    *visibility = Visibility::Inherited;
                }
            }
            continue;
        }
        let radius = if spec.is_lead { LEAD_RADIUS } else { RADIUS };
        let color = spec.state.color();
        let core = materials.add(ColorMaterial::from_color(Color::from(color)));
        let halo = materials.add(ColorMaterial::from_color(Color::from(color.with_alpha(0.0))));
        commands
            .spawn((
                Node {
                    key: spec.key.clone(),
                    is_lead: spec.is_lead,
                    state: spec.state,
                    target,
                    color,
                    appear: 0.0,
                    hover: 0.0,
                    core: core.clone(),
                    halo: halo.clone(),
                },
                Transform::from_translation(Vec3::new(0.0, target.y + 40.0, 1.0)).with_scale(Vec3::splat(0.01)),
                Visibility::default(),
            ))
            .with_children(|p| {
                p.spawn((Halo, Mesh2d(meshes.add(Circle::new(radius * 1.5))), MeshMaterial2d(halo), Transform::from_xyz(0.0, 0.0, -0.2)));
                p.spawn((
                    Mesh2d(meshes.add(Circle::new(radius + 3.0))),
                    MeshMaterial2d(materials.add(ColorMaterial::from_color(Color::srgba(1.0, 1.0, 1.0, 0.85)))),
                    Transform::from_xyz(0.0, 0.0, -0.1),
                ));
                p.spawn((Core, Mesh2d(meshes.add(Circle::new(radius))), MeshMaterial2d(core), Transform::default()));
                p.spawn((
                    Label::Initials,
                    Text2d::new(initials(&spec.title)),
                    TextFont { font_size: FontSize::Px(if spec.is_lead { 24.0 } else { 20.0 }), ..default() },
                    TextColor(Color::WHITE),
                    Transform::from_xyz(0.0, 0.0, 0.5),
                ));
                p.spawn((
                    Label::Title,
                    Text2d::new(spec.title.clone()),
                    TextFont { font_size: FontSize::Px(17.0), ..default() },
                    TextColor(Color::srgb(0.92, 0.93, 0.96)),
                    Transform::from_xyz(0.0, -radius - 20.0, 0.5),
                ));
                p.spawn((
                    Label::Subtitle,
                    Text2d::new(spec.subtitle.clone()),
                    TextFont { font_size: FontSize::Px(13.0), ..default() },
                    TextColor(Color::srgb(0.60, 0.63, 0.70)),
                    Transform::from_xyz(0.0, -radius - 40.0, 0.5),
                ));
                p.spawn((Preview::default(), Sprite::default(), Visibility::Hidden, Transform::from_xyz(0.0, -radius - 54.0, 0.6)));
            });
    }
}

fn initials(name: &str) -> String {
    let words: Vec<&str> = name.split(|c: char| c.is_whitespace() || c == '-' || c == '_').filter(|w| !w.is_empty()).collect();
    let picked: String = match words.as_slice() {
        [one] => one.chars().take(2).collect(),
        many => many.iter().filter_map(|w| w.chars().next()).take(2).collect(),
    };
    picked.to_uppercase()
}

fn lerp_color(a: LinearRgba, b: LinearRgba, t: f32) -> LinearRgba {
    LinearRgba::new(a.red + (b.red - a.red) * t, a.green + (b.green - a.green) * t, a.blue + (b.blue - a.blue) * t, a.alpha + (b.alpha - a.alpha) * t)
}

/// Overshooting ease for the pop-in.
fn ease_out_back(t: f32) -> f32 {
    let (c1, c3) = (1.70158, 2.70158);
    1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
}

fn animate_nodes(
    time: Res<Time>,
    input: Res<SceneInput>,
    mut nodes: Query<(&mut Node, &mut Transform, &Children), Without<Halo>>,
    mut halos: Query<&mut Transform, With<Halo>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
) {
    let dt = time.delta_secs();
    let t = time.elapsed_secs();
    let follow = 1.0 - (-dt * 7.0).exp();
    for (mut node, mut transform, children) in &mut nodes {
        node.appear = (node.appear + dt * 1.8).min(1.0);
        // Idle agents float gently around their slot; working ones bob faster.
        let bob = match node.state {
            NodeState::Working => (t * 3.0 + node.target.x * 0.01).sin() * 4.0,
            NodeState::Off => 0.0,
            _ => (t * 1.2 + node.target.x * 0.01).sin() * 2.5,
        };
        let pos = transform.translation.truncate().lerp(node.target + Vec2::Y * bob, follow);
        transform.translation = pos.extend(1.0);
        let selected = input.selected == node.key;
        let grow = 1.0 + node.hover * 0.08 + if selected { 0.06 } else { 0.0 };
        transform.scale = Vec3::splat(ease_out_back(node.appear).max(0.01) * grow);

        let target_color = node.state.color();
        node.color = lerp_color(node.color, target_color, 1.0 - (-dt * 4.0).exp());
        if let Some(mut core) = materials.get_mut(&node.core) {
            core.color = Color::from(node.color);
        }
        let (alpha, scale) = match node.state {
            NodeState::Working => (0.30 + 0.12 * (t * 4.0).sin(), 1.0 + 0.12 * (t * 4.0).sin()),
            NodeState::Starting => (0.22 + 0.10 * (t * 6.0).sin(), 0.9 + 0.1 * (t * 6.0).sin()),
            NodeState::Failed => (0.18 + 0.12 * (t * 9.0).sin().max(0.0), 0.9),
            NodeState::Idle | NodeState::Done => (0.14 + 0.03 * (t * 1.5).sin(), 0.85),
            NodeState::Off => (0.0, 0.7),
        };
        if let Some(mut halo) = materials.get_mut(&node.halo) {
            halo.color = Color::from(node.color.with_alpha(alpha.max(0.0) + if selected { 0.08 } else { 0.0 }));
        }
        for child in children.iter() {
            if let Ok(mut h) = halos.get_mut(child) {
                h.scale = Vec3::splat(scale);
            }
        }
    }
}

fn node_pos(nodes: &Query<(&Node, &Transform)>, key: &str) -> Option<(Vec2, LinearRgba)> {
    nodes.iter().find(|(n, _)| n.key == key).map(|(n, t)| (t.translation.truncate(), n.color))
}

fn spawn_orbs(mut input: ResMut<SceneInput>, mut effects: ResMut<Effects>, nodes: Query<(&Node, &Transform)>) {
    for (from, to) in std::mem::take(&mut input.messages) {
        let color = if from == YOU {
            Srgba::rgb(0.55, 0.75, 1.0).into()
        } else {
            node_pos(&nodes, &from).map_or(Srgba::rgb(1.0, 0.85, 0.4).into(), |(_, c)| lerp_color(c, LinearRgba::WHITE, 0.35))
        };
        effects.orbs.push(Orb { from, to, t: 0.0, color });
    }
}

/// Where links leave the lead: under its labels, so they never cross its text.
const LEAD_PORT: f32 = LEAD_RADIUS + 50.0;

/// The y of the horizontal bus, halfway between the lead's port and the top of the member row. Taken from the
/// targets, not the bobbing positions, so the bus stays perfectly straight.
fn bus_y(nodes: &Query<(&Node, &Transform)>) -> Option<f32> {
    let lead = nodes.iter().find(|(n, _)| n.is_lead)?.0.target.y - LEAD_PORT;
    let row = nodes.iter().filter(|(n, _)| !n.is_lead).map(|(n, _)| n.target.y + RADIUS).reduce(f32::max)?;
    Some((lead + row) / 2.0)
}

/// Org-chart routing: straight down out of `a`, along the bus, straight down (or up) into `b`.
fn elbow(a: Vec2, b: Vec2, bus: f32) -> Vec<Vec2> {
    vec![a, Vec2::new(a.x, bus), Vec2::new(b.x, bus), b]
}

/// The point at fraction `u` of a polyline's length, so packets and orbs move at an even speed round corners.
fn along(points: &[Vec2], u: f32) -> Vec2 {
    let total: f32 = points.windows(2).map(|w| w[0].distance(w[1])).sum();
    let mut left = u.clamp(0.0, 1.0) * total;
    for w in points.windows(2) {
        let len = w[0].distance(w[1]);
        if left <= len && len > 0.0 {
            return w[0].lerp(w[1], left / len);
        }
        left -= len;
    }
    *points.last().unwrap_or(&Vec2::ZERO)
}

/// The path a message takes between two nodes, following the chart's lines; a message from the user drops in from
/// the top edge.
fn route(nodes: &Query<(&Node, &Transform)>, from: &str, to: &str, top: f32) -> Option<Vec<Vec2>> {
    let find = |key: &str| nodes.iter().find(|(n, _)| n.key == key).map(|(n, t)| (n.is_lead, t.translation.truncate()));
    let (to_lead, b) = find(to)?;
    if from == YOU {
        let entry = b + Vec2::Y * if to_lead { LEAD_RADIUS } else { RADIUS };
        return Some(vec![Vec2::new(b.x, top), entry]);
    }
    let (from_lead, a) = find(from)?;
    let port = |is_lead: bool, p: Vec2| if is_lead { p - Vec2::Y * LEAD_PORT } else { p + Vec2::Y * RADIUS };
    Some(elbow(port(from_lead, a), port(to_lead, b), bus_y(nodes)?))
}

fn animate_effects(
    time: Res<Time>,
    camera: Single<&Camera, With<SceneCamera>>,
    mut effects: ResMut<Effects>,
    nodes: Query<(&Node, &Transform)>,
    zoom: Res<Zoom>,
) {
    let dt = time.delta_secs();
    let size = view_size(&camera, &zoom);
    let mut sparks = Vec::new();
    let mut ripples = Vec::new();
    effects.orbs.retain_mut(|orb| {
        let Some(path) = route(&nodes, &orb.from, &orb.to, size.y / 2.0 + 20.0) else { return false };
        orb.t += dt / 1.3;
        let eased = orb.t.min(1.0);
        let eased = eased * eased * (3.0 - 2.0 * eased);
        sparks.push(Spark { pos: along(&path, eased), age: 0.0, color: orb.color });
        if orb.t >= 1.0 {
            let target = node_pos(&nodes, &orb.to).map_or(*path.last().unwrap(), |p| p.0);
            ripples.push(Ripple { pos: target, age: 0.0, color: orb.color });
            return false;
        }
        true
    });
    effects.sparks.extend(sparks);
    effects.ripples.extend(ripples);
    effects.sparks.retain_mut(|s| {
        s.age += dt;
        s.age < 0.6
    });
    effects.ripples.retain_mut(|r| {
        r.age += dt;
        r.age < 0.9
    });
}

/// The world point under a window position, for the camera `fit_camera` sets this frame.
fn to_world(cursor: Vec2, viewport: Rect, scale: f32, pan: Vec2) -> Vec2 {
    pan + (cursor - viewport.center()) * Vec2::new(scale, -scale)
}

/// Mouse on the scene: the wheel zooms around the cursor, a left drag pans, and a left click (press and release
/// without dragging) on an agent picks it. A right press reports what is under the cursor for the context menu.
fn pointer(
    window: Single<&Window, With<PrimaryWindow>>,
    camera: Single<&Camera, With<SceneCamera>>,
    buttons: Res<ButtonInput<MouseButton>>,
    scroll: Res<AccumulatedMouseScroll>,
    time: Res<Time>,
    fit: Res<Zoom>,
    mut view: ResMut<UserView>,
    mut nodes: Query<(&mut Node, &Transform)>,
    mut output: ResMut<SceneOutput>,
) {
    let Some(rect) = camera.logical_viewport_rect() else { return };
    let cursor = window.cursor_position();
    let inside = cursor.filter(|c| rect.contains(*c));

    if let Some(c) = inside
        && scroll.delta.y != 0.0
    {
        let steps = match scroll.unit {
            MouseScrollUnit::Line => scroll.delta.y,
            MouseScrollUnit::Pixel => scroll.delta.y / 40.0,
        };
        let before = to_world(c, rect, fit.0 * view.zoom, view.pan);
        view.zoom = (view.zoom * 0.87_f32.powf(steps)).clamp(0.2, 5.0);
        let after = to_world(c, rect, fit.0 * view.zoom, view.pan);
        view.pan += before - after;
    }

    if buttons.just_pressed(MouseButton::Left) {
        view.press = inside;
        view.dragging = false;
        if let Some(c) = inside {
            view.last = c;
        }
    }
    if let (Some(start), Some(c)) = (view.press, cursor)
        && buttons.pressed(MouseButton::Left)
    {
        view.dragging |= c.distance(start) > 4.0;
        if view.dragging {
            let scale = fit.0 * view.zoom;
            let moved = (c - view.last) * Vec2::new(scale, -scale);
            view.pan -= moved;
        }
        view.last = c;
    }
    let clicked = buttons.just_released(MouseButton::Left) && view.press.is_some() && !view.dragging;
    if buttons.just_released(MouseButton::Left) {
        view.press = None;
    }

    let right = inside.is_some() && buttons.just_pressed(MouseButton::Right);
    if right {
        output.right_clicked = Some(None);
    }
    let world = inside.filter(|_| !view.dragging).map(|c| to_world(c, rect, fit.0 * view.zoom, view.pan));
    let dt = time.delta_secs();
    for (mut node, transform) in &mut nodes {
        let radius = if node.is_lead { LEAD_RADIUS } else { RADIUS };
        let over = world.is_some_and(|w| w.distance(transform.translation.truncate()) <= radius + 6.0);
        node.hover = (node.hover + if over { dt * 8.0 } else { -dt * 8.0 }).clamp(0.0, 1.0);
        if over && clicked {
            output.clicked = Some(node.key.clone());
        }
        if over && right {
            output.right_clicked = Some(Some(node.key.clone()));
        }
    }
}

fn draw(
    mut gizmos: Gizmos,
    time: Res<Time>,
    input: Res<SceneInput>,
    camera: Single<&Camera, With<SceneCamera>>,
    effects: Res<Effects>,
    nodes: Query<(&Node, &Transform)>,
    zoom: Res<Zoom>,
) {
    let t = time.elapsed_secs();
    let size = view_size(&camera, &zoom);

    // Gizmos draw over the meshes, so stars keep clear of each node and its labels.
    let clear: Vec<Vec2> = nodes.iter().map(|(_, t)| t.translation.truncate()).collect();
    for (unit, phase) in &effects.stars {
        let p = Vec2::new(unit.x * size.x, unit.y * size.y);
        if clear.iter().any(|c| (p.x - c.x).abs() < 75.0 && p.y < c.y + 60.0 && p.y > c.y - 110.0) {
            continue;
        }
        let twinkle = 0.25 + 0.2 * (t * 0.8 + phase).sin();
        gizmos.circle_2d(p, 0.8, Color::srgba(0.75, 0.82, 1.0, twinkle));
    }

    let lead = nodes.iter().find(|(n, _)| n.is_lead);
    if let (Some((lead, lead_t)), Some(bus)) = (lead, bus_y(&nodes)) {
        let start = lead_t.translation.truncate() - Vec2::Y * LEAD_PORT;
        let idle = LinearRgba::new(0.42, 0.45, 0.55, 0.75);
        // Every member's elbow shares the trunk and the bus, so the idle lines go first and busy ones on top.
        let mut links: Vec<(&Node, Vec<Vec2>, bool)> = nodes
            .iter()
            .filter(|(n, _)| !n.is_lead)
            .map(|(n, t)| {
                let path = elbow(start, t.translation.truncate() + Vec2::Y * RADIUS, bus);
                let active = n.state == NodeState::Working || lead.state == NodeState::Working;
                (n, path, active)
            })
            .collect();
        links.sort_by_key(|(_, _, active)| *active);
        for (node, path, active) in &links {
            let color = if *active { lerp_color(idle, node.color.with_alpha(0.9), 0.7) } else { idle };
            gizmos.linestrip_2d(path.clone(), Color::from(color));
            if *active {
                // Packets flowing along the link while either end is busy.
                for k in 0..3 {
                    let u = (t * 0.35 + k as f32 / 3.0).fract();
                    gizmos.circle_2d(along(path, u), 2.5, Color::from(node.color.with_alpha(0.95)));
                }
            }
        }
        // A small junction dot where the trunk meets the bus.
        gizmos.circle_2d(Vec2::new(start.x, bus), 2.0, Color::from(idle));
    }

    for (node, transform) in &nodes {
        let p = transform.translation.truncate();
        let radius = (if node.is_lead { LEAD_RADIUS } else { RADIUS }) * transform.scale.x;
        if node.state == NodeState::Starting {
            let iso = Isometry2d::new(p, Rot2::radians(t * 4.0));
            gizmos.arc_2d(iso, TAU * 0.3, radius + 10.0, Color::from(node.color));
            gizmos.arc_2d(Isometry2d::new(p, Rot2::radians(t * 4.0 + TAU / 2.0)), TAU * 0.3, radius + 10.0, Color::from(node.color));
        }
        if input.selected == node.key {
            let pulse = 0.5 + 0.5 * (t * 3.0).sin();
            gizmos.circle_2d(p, radius + 12.0 + pulse * 2.0, Color::srgba(1.0, 1.0, 1.0, 0.55 + 0.3 * pulse));
        }
        if node.hover > 0.01 {
            gizmos.circle_2d(p, radius + 7.0, Color::srgba(1.0, 1.0, 1.0, 0.35 * node.hover));
        }
    }

    for s in &effects.sparks {
        let life = 1.0 - s.age / 0.6;
        for r in [1.0, 2.5, 4.0] {
            gizmos.circle_2d(s.pos, r * (0.4 + life * 0.8), Color::from(s.color.with_alpha(life * 0.8)));
        }
    }
    for r in &effects.ripples {
        let k = r.age / 0.9;
        gizmos.circle_2d(r.pos, RADIUS + 6.0 + k * 40.0, Color::from(r.color.with_alpha((1.0 - k) * 0.9)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_puts_lead_on_top_and_members_in_order() {
        let size = Vec2::new(900.0, 600.0);
        let lead = layout(true, 0, 3, size);
        let row: Vec<Vec2> = (0..3).map(|i| layout(false, i, 3, size)).collect();
        assert!(row.iter().all(|p| p.y < lead.y));
        assert!(row[0].x < row[1].x && row[1].x < row[2].x);
        assert_eq!(layout(false, 0, 1, size).x, 0.0);
    }

    #[test]
    fn along_walks_round_the_elbow_at_even_speed() {
        let path = elbow(Vec2::new(0.0, 100.0), Vec2::new(100.0, 0.0), 50.0);
        // 50 down, 100 across, 50 down: the halfway point is the middle of the bus.
        assert_eq!(along(&path, 0.5), Vec2::new(50.0, 50.0));
        assert_eq!(along(&path, 0.0), Vec2::new(0.0, 100.0));
        assert_eq!(along(&path, 1.0), Vec2::new(100.0, 0.0));
    }

    #[test]
    fn to_world_centres_on_pan_with_y_up() {
        let viewport = Rect::new(200.0, 30.0, 1000.0, 630.0);
        assert_eq!(to_world(viewport.center(), viewport, 2.0, Vec2::new(5.0, 7.0)), Vec2::new(5.0, 7.0));
        // 10 px right and 10 px down on screen, at 2 world units per pixel.
        assert_eq!(to_world(viewport.center() + Vec2::new(10.0, 10.0), viewport, 2.0, Vec2::ZERO), Vec2::new(20.0, -20.0));
    }

    #[test]
    fn initials_handle_dashes_and_single_words() {
        assert_eq!(initials("cf-ann"), "CA");
        assert_eq!(initials("Alice"), "AL");
        assert_eq!(initials("Team lead"), "TL");
    }
}

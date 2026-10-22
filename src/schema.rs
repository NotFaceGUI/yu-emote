//! Typed E-mote model schema.
//!
//! The PSB tree is a generic value container. This module assigns names and
//! types to the parts of the tree that are **observed in official data** and
//! leaves everything else available as [`serde_json::Value`] through the
//! [`raw`](Controller::raw) / [`metadata_raw`](Model::metadata_raw) fields.
//!
//! Evidence for the shape (keys, value kinds) is the official YU-RIS sample
//! `emote_test.psb` (PSB v2) plus the schema key strings embedded in the
//! official `emotedriver.dll`:
//!
//! ```text
//! emote / motion / partsControl / timelineControl / variableList /
//! motionList / inheritMask / meshTransform / meshSyncChildMask /
//! meshDivision / meshCombine / motionIndependentLayerInherit /
//! particle* / stencilCompositeMaskLayerList / mask / mesh / model / variable
//! ```
//!
//! Numeric *semantics* that are not evidenced are kept as raw values and are
//! not interpreted by this layer.

use std::collections::{BTreeMap, HashMap};

use emote_psb::value::{PsbValue, number::PsbNumber};
use serde::Serialize;
use serde_json::Value as JsonValue;

use crate::psb::PsbDocument;

type PsbMap = HashMap<smol_str::SmolStr, PsbValue>;

/// Return the object map of a value, if it is an object.
pub fn as_object(value: &PsbValue) -> Option<&PsbMap> {
    match value {
        PsbValue::Object(map) => Some(map),
        _ => None,
    }
}

/// Look up a key in an object value.
pub fn field<'a>(value: &'a PsbValue, key: &str) -> Option<&'a PsbValue> {
    as_object(value)?.get(key)
}

/// List items of a value, if it is a list.
pub fn as_list(value: &PsbValue) -> Option<&[PsbValue]> {
    match value {
        PsbValue::List(items) => Some(items),
        _ => None,
    }
}

/// Integer view of a numeric value.
pub fn as_i64(value: &PsbValue) -> Option<i64> {
    match value {
        PsbValue::Number(PsbNumber::Integer(value)) => Some(*value),
        PsbValue::Number(PsbNumber::Double(value)) => Some(*value as i64),
        PsbValue::Number(PsbNumber::Float(value)) => Some(*value as i64),
        _ => None,
    }
}

/// Floating-point view of a numeric value.
pub fn as_f64(value: &PsbValue) -> Option<f64> {
    match value {
        PsbValue::Number(PsbNumber::Integer(value)) => Some(*value as f64),
        PsbValue::Number(PsbNumber::Double(value)) => Some(*value),
        PsbValue::Number(PsbNumber::Float(value)) => Some(f64::from(*value)),
        _ => None,
    }
}

/// String view of a string value.
pub fn as_str(value: &PsbValue) -> Option<&str> {
    match value {
        PsbValue::String(value) => Some(value),
        _ => None,
    }
}

/// Boolean view of a boolean value.
pub fn as_bool(value: &PsbValue) -> Option<bool> {
    match value {
        PsbValue::Bool(value) => Some(*value),
        _ => None,
    }
}

/// Convert a PSB value into a deterministic JSON tree.
///
/// Objects are sorted by key (via `serde_json`'s ordered map), lists keep
/// their order. Resource references become `{"$resource": n}` and compiler
/// intrinsics become `{"$intrinsic": "..."}` so they cannot be confused with
/// ordinary data.
pub fn value_to_json(value: &PsbValue) -> JsonValue {
    match value {
        PsbValue::Null => JsonValue::Null,
        PsbValue::Bool(value) => JsonValue::Bool(*value),
        PsbValue::Number(PsbNumber::Integer(value)) => JsonValue::from(*value),
        PsbValue::Number(PsbNumber::Double(value)) => float_json(*value),
        PsbValue::Number(PsbNumber::Float(value)) => float_json(f64::from(*value)),
        PsbValue::String(value) => JsonValue::String(value.to_string()),
        PsbValue::Resource(index) => {
            let mut map = serde_json::Map::new();
            map.insert("$resource".into(), JsonValue::from(*index));
            JsonValue::Object(map)
        }
        PsbValue::ExtraResource(index) => {
            let mut map = serde_json::Map::new();
            map.insert("$extra_resource".into(), JsonValue::from(*index));
            JsonValue::Object(map)
        }
        PsbValue::List(items) => JsonValue::Array(items.iter().map(value_to_json).collect()),
        PsbValue::Object(map) => {
            let mut out = serde_json::Map::new();
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort();
            for key in keys {
                out.insert(key.to_string(), value_to_json(&map[key]));
            }
            JsonValue::Object(out)
        }
        PsbValue::CompilerNumber => intrinsic("number"),
        PsbValue::CompilerString => intrinsic("string"),
        PsbValue::CompilerResource => intrinsic("resource"),
        PsbValue::CompilerDecimal => intrinsic("decimal"),
        PsbValue::CompilerArray => intrinsic("array"),
        PsbValue::CompilerBool => intrinsic("bool"),
        PsbValue::CompilerBinaryTree => intrinsic("binary_tree"),
    }
}

fn intrinsic(name: &str) -> JsonValue {
    let mut map = serde_json::Map::new();
    map.insert("$intrinsic".into(), JsonValue::String(name.into()));
    JsonValue::Object(map)
}

fn float_json(value: f64) -> JsonValue {
    serde_json::Number::from_f64(value).map_or(JsonValue::Null, JsonValue::Number)
}

fn get<'a>(map: &'a PsbMap, key: &str) -> Option<&'a PsbValue> {
    map.get(key)
}

fn get_i64(map: &PsbMap, key: &str) -> Option<i64> {
    get(map, key).and_then(as_i64)
}

fn get_f64(map: &PsbMap, key: &str) -> Option<f64> {
    get(map, key).and_then(as_f64)
}

fn get_str(map: &PsbMap, key: &str) -> Option<String> {
    get(map, key).and_then(as_str).map(str::to_owned)
}

fn get_bool(map: &PsbMap, key: &str) -> Option<bool> {
    get(map, key).and_then(as_bool)
}

fn empty_map() -> &'static PsbMap {
    static EMPTY: std::sync::OnceLock<PsbMap> = std::sync::OnceLock::new();
    EMPTY.get_or_init(PsbMap::new)
}

fn obj_or_empty(value: Option<&PsbValue>) -> &PsbMap {
    match value.and_then(as_object) {
        Some(map) => map,
        None => empty_map(),
    }
}

/// Screen geometry from the model's `screenSize` section.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ScreenSize {
    /// Width in model units.
    pub width: f64,
    /// Height in model units.
    pub height: f64,
    /// Origin X in model units.
    pub origin_x: f64,
    /// Origin Y in model units.
    pub origin_y: f64,
}

/// One texture atlas page (`source/tex#N`).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct TextureSource {
    /// Page name as it appears in `source` (for example `tex#0`).
    pub name: String,
    /// `source/tex#N/type` (observed value `0`).
    pub kind: Option<i64>,
    /// Width in pixels (`texture/width`).
    pub width: u32,
    /// Height in pixels (`texture/height`).
    pub height: u32,
    /// Dimensions actually used by the atlas (`texture/truncated_width`).
    pub truncated_width: u32,
    /// Dimensions actually used by the atlas (`texture/truncated_height`).
    pub truncated_height: u32,
    /// Pixel format name (`texture/type`, observed `RGBA4444`).
    pub format: Option<String>,
    /// Resource index for the pixel bytes (`texture/pixel`).
    pub resource: Option<u32>,
    /// Icon rectangles keyed by numeric icon id, sorted.
    pub icons: BTreeMap<u32, AtlasIcon>,
    /// Icon entries whose keys are not numeric, sorted lexicographically.
    pub named_icons: BTreeMap<String, AtlasIcon>,
}

impl TextureSource {
    /// Look up an icon by numeric id.
    pub fn icon(&self, id: u32) -> Option<&AtlasIcon> {
        self.icons.get(&id)
    }
}

/// One atlas rectangle (`source/tex#N/icon/<id>`).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct AtlasIcon {
    /// `attr`; semantics not evidenced yet.
    pub attr: Option<i64>,
    /// Source `left` in pixels.
    pub left: f64,
    /// Source `top` in pixels.
    pub top: f64,
    /// Source `width` in pixels.
    pub width: f64,
    /// Source `height` in pixels.
    pub height: f64,
    /// Pivot X inside the rectangle.
    pub origin_x: f64,
    /// Pivot Y inside the rectangle.
    pub origin_y: f64,
}

impl AtlasIcon {
    /// Pivot in atlas pixel space.
    pub fn pivot(&self) -> [f64; 2] {
        [self.left + self.origin_x, self.top + self.origin_y]
    }
}

/// Model metadata (`metadata`).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Metadata {
    /// `metadata/format` (observed `emote`).
    pub format: Option<String>,
    /// `metadata/base/chara`.
    pub character: Option<String>,
    /// `metadata/base/motion`.
    pub base_motion: Option<String>,
    /// `metadata/version`.
    pub version: Option<i64>,
    /// `metadata/scale`.
    pub scale: Option<i64>,
    /// `metadata/mirror`.
    pub mirror: Option<i64>,
    /// Complete metadata tree, sorted.
    pub raw: JsonValue,
}

/// One controller definition with its full raw payload.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Controller {
    /// Display label.
    pub label: Option<String>,
    /// `enabled` flag when present.
    pub enabled: Option<bool>,
    /// Controller parameter set name (`parameter`), when present.
    pub parameter: Option<String>,
    /// Left/right input variable (`var_lr`), when present.
    pub variable_lr: Option<String>,
    /// Up/down input variable (`var_ud`), when present.
    pub variable_ud: Option<String>,
    /// Mixed left/right input variable (`var_lrm`), when present.
    pub variable_lrm: Option<String>,
    /// Complete controller payload, sorted.
    pub raw: JsonValue,
}

/// All controller lists exposed by the model metadata.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Controllers {
    /// Bust physics controllers.
    pub bust: Vec<Controller>,
    /// Hair physics controllers.
    pub hair: Vec<Controller>,
    /// Parts physics controllers.
    pub parts: Vec<Controller>,
    /// Eye controllers (blink).
    pub eye: Vec<Controller>,
    /// Eyebrow controllers.
    pub eyebrow: Vec<Controller>,
    /// Mouth controllers.
    pub mouth: Vec<Controller>,
    /// Timeline controllers.
    pub timeline: Vec<Controller>,
    /// Transition controllers.
    pub transition: Vec<Controller>,
    /// Clamp controllers.
    pub clamp: Vec<Controller>,
    /// Loop controllers.
    pub loop_control: Vec<Controller>,
}

/// A named variable with discrete state frames (`metadata/variableList`).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct VariableDef {
    /// Variable name.
    pub label: String,
    /// Discrete states: frame number and state label.
    pub states: Vec<VariableState>,
}

/// One discrete variable state.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct VariableState {
    /// Frame number (observed negative values, meaning "before time 0").
    pub frame: i64,
    /// State label selected at this frame.
    pub label: String,
}

/// A timeline (`metadata/timelineControl` entry).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Timeline {
    /// Timeline name.
    pub label: String,
    /// `lastTime` (one past the final frame, observed `61` for 61 frames).
    pub last_time: i64,
    /// Loop start frame.
    pub loop_begin: i64,
    /// Loop end frame.
    pub loop_end: i64,
    /// Per-variable tracks.
    pub tracks: Vec<TimelineTrack>,
}

/// One variable track inside a timeline.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct TimelineTrack {
    /// Variable name (matches [`VariableDef::label`] or a controller input).
    pub label: String,
    /// Keyframes, sorted by time as stored in the model.
    pub frames: Vec<TimelineFrame>,
}

/// One timeline keyframe.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct TimelineFrame {
    /// Frame number.
    pub time: i64,
    /// Frame kind (`type`); semantics not fully evidenced.
    pub kind: i64,
    /// Easing selector (`content/easing`), when present.
    pub easing: Option<i64>,
    /// Target value (`content/value`).
    pub value: f64,
}

/// One object definition (`object/<name>`).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ObjectDef {
    /// Object name.
    pub name: String,
    /// `type` field.
    pub kind: Option<i64>,
    /// Motions keyed by name, sorted.
    pub motions: BTreeMap<String, Motion>,
}

/// A motion (`object/<name>/motion/<motion>`).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Motion {
    /// `lastTime`.
    pub last_time: i64,
    /// `bounds`.
    pub bounds: Bounds,
    /// Layer label to index map (`layerIndexMap`).
    pub layer_index_map: BTreeMap<String, i64>,
    /// Root layers.
    pub layers: Vec<Layer>,
}

impl Motion {
    /// Number of layers in this motion, including descendants.
    pub fn layer_count(&self) -> usize {
        count_layers(&self.layers)
    }
}

/// Motion bounds (`bounds`).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Bounds {
    /// Left.
    pub left: f64,
    /// Top.
    pub top: f64,
    /// Right.
    pub right: f64,
    /// Bottom.
    pub bottom: f64,
}

/// One layer node.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Layer {
    /// Display label.
    pub label: String,
    /// Layer kind (`type`): observed values 0, 1, 2, 3, 12.
    pub kind: i64,
    /// `coordinate`.
    pub coordinate: Option<i64>,
    /// `exportSelf`.
    pub export_self: Option<i64>,
    /// `groundCorrection`.
    pub ground_correction: Option<i64>,
    /// `inheritMask` bit set.
    pub inherit_mask: Option<i64>,
    /// `joinTarget`.
    pub join_target: Option<i64>,
    /// `meshCombine`.
    pub mesh_combine: Option<i64>,
    /// `meshTransform`.
    pub mesh_transform: Option<i64>,
    /// `meshSyncChildMask`.
    pub mesh_sync_child_mask: Option<i64>,
    /// `meshDivision` (observed 20).
    pub mesh_division: Option<i64>,
    /// `parameterize`.
    pub parameterize: Option<i64>,
    /// `stencilType`.
    pub stencil_type: Option<i64>,
    /// `objTriPriority`.
    pub obj_tri_priority: Option<i64>,
    /// Keyframes.
    pub frames: Vec<Frame>,
    /// Child layers.
    pub children: Vec<Layer>,
}

/// One layer keyframe.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Frame {
    /// Frame number.
    pub time: i64,
    /// Frame kind (`type`): observed values 0, 2, 3.
    pub kind: i64,
    /// Keyframe content; absent for terminal frames.
    pub content: Option<Content>,
}

/// A keyframe payload (`content`).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Content {
    /// Texture page name (`src`), for example `tex#0` or `blank`.
    pub src: Option<String>,
    /// Atlas icon reference (`icon`).
    pub icon: Option<IconRef>,
    /// Mask bits (`mask`).
    pub mask: Option<i64>,
    /// Position (`coord`), first two components.
    pub coord: Option<[f64; 2]>,
    /// Rotation (`angle`) in degrees, as stored.
    pub angle: Option<f64>,
    /// Color (`color`), up to four components.
    pub color: Option<[f64; 4]>,
    /// Opacity (`opa`).
    pub opa: Option<f64>,
    /// Mesh control points (`mesh`).
    pub mesh: Option<Mesh>,
    /// Referenced sub-motion (`motion`), for layer kind 3.
    pub motion: Option<String>,
}

/// An atlas icon reference. The official content stores either a numeric icon
/// id or a `left:top:width:height` string.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct IconRef {
    /// Original value.
    pub raw: String,
    /// Numeric id when the value is a plain integer.
    pub id: Option<u32>,
    /// Rectangle when the value uses the `left:top:width:height` form.
    pub rect: Option<[f64; 4]>,
}

/// Mesh control points (`mesh`).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Mesh {
    /// Flat `pairs` list from `bp` (16 × 2 floats observed).
    pub bp: Option<Vec<f64>>,
    /// Flat `pairs` list from `cc`.
    pub cc: Option<Vec<f64>>,
}

impl Mesh {
    /// `bp` interpreted as 2D control points.
    pub fn bp_points(&self) -> Vec<[f64; 2]> {
        points(self.bp.as_deref())
    }

    /// `cc` interpreted as 2D control points.
    pub fn cc_points(&self) -> Vec<[f64; 2]> {
        points(self.cc.as_deref())
    }
}

fn points(flat: Option<&[f64]>) -> Vec<[f64; 2]> {
    match flat {
        Some(values) => values
            .chunks_exact(2)
            .map(|pair| [pair[0], pair[1]])
            .collect(),
        None => Vec::new(),
    }
}

/// Deterministic structural counts for one model.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ModelStats {
    /// Number of texture pages.
    pub sources: usize,
    /// Number of atlas icons across all pages.
    pub icons: usize,
    /// Number of objects.
    pub objects: usize,
    /// Number of motions.
    pub motions: usize,
    /// Number of layers (recursive).
    pub layers: usize,
    /// Number of layer keyframes (non-recursive).
    pub frames: usize,
    /// Number of timelines.
    pub timelines: usize,
    /// Number of variables.
    pub variables: usize,
    /// Number of embedded resources.
    pub resources: usize,
}

/// A parsed E-mote model.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Model {
    /// `id` (observed `motion`).
    pub id: Option<String>,
    /// `label` (observed `template`).
    pub label: Option<String>,
    /// `spec`.
    pub spec: Option<String>,
    /// `version` (model format version, observed `1.0`).
    pub version: Option<f64>,
    /// Screen geometry.
    pub screen: ScreenSize,
    /// Metadata.
    pub metadata: Metadata,
    /// Texture atlas pages, sorted by name.
    pub sources: Vec<TextureSource>,
    /// Objects keyed by name, sorted.
    pub objects: BTreeMap<String, ObjectDef>,
    /// Variables.
    pub variables: Vec<VariableDef>,
    /// Timelines.
    pub timelines: Vec<Timeline>,
    /// Controllers.
    pub controllers: Controllers,
    /// Complete model tree, sorted. Use this for fields without typed access.
    pub raw: JsonValue,
    /// Structural counts.
    pub stats: ModelStats,
}

impl Model {
    /// Extract the typed model from a parsed PSB document.
    pub fn from_document(document: &PsbDocument) -> Self {
        let root = &document.root;
        let mut model = Model {
            id: field(root, "id").and_then(as_str).map(str::to_owned),
            label: field(root, "label").and_then(as_str).map(str::to_owned),
            spec: field(root, "spec").and_then(as_str).map(str::to_owned),
            version: field(root, "version").and_then(as_f64),
            screen: screen_size(field(root, "screenSize")),
            metadata: Metadata::default(),
            sources: Vec::new(),
            objects: BTreeMap::new(),
            variables: Vec::new(),
            timelines: Vec::new(),
            controllers: Controllers::default(),
            raw: JsonValue::Null,
            stats: ModelStats {
                resources: document.resource_data.len(),
                ..ModelStats::default()
            },
        };

        if let Some(metadata) = field(root, "metadata") {
            let map = obj_or_empty(Some(metadata));
            let base = obj_or_empty(get(map, "base"));
            model.metadata = Metadata {
                format: get_str(map, "format"),
                character: get_str(base, "chara"),
                base_motion: get_str(base, "motion"),
                version: get_i64(map, "version"),
                scale: get_i64(map, "scale"),
                mirror: get_i64(map, "mirror"),
                raw: value_to_json(metadata),
            };
            model.variables = variables(map);
            model.timelines = timelines(map);
            model.controllers = controllers(map);
        }

        if let Some(source) = field(root, "source") {
            let mut pages: Vec<_> = obj_or_empty(Some(source))
                .iter()
                .filter(|(name, _)| name.starts_with("tex#"))
                .map(|(name, page)| texture_source(name, page))
                .collect();
            pages.sort_by(|a, b| a.name.cmp(&b.name));
            for page in pages {
                model.stats.icons += page.icons.len() + page.named_icons.len();
                model.stats.sources += 1;
                model.sources.push(page);
            }
        }

        if let Some(objects) = field(root, "object") {
            for (name, object) in objects_map(objects) {
                let object_map = obj_or_empty(Some(object));
                let mut object_def = ObjectDef {
                    name: name.clone(),
                    kind: get_i64(object_map, "type"),
                    motions: BTreeMap::new(),
                };
                if let Some(motions) = get(object_map, "motion") {
                    for (motion_name, motion_value) in obj_or_empty(Some(motions)) {
                        let motion_def = motion(motion_value);
                        model.stats.motions += 1;
                        model.stats.layers += count_layers(&motion_def.layers);
                        object_def
                            .motions
                            .insert(motion_name.to_string(), motion_def);
                    }
                }
                model.stats.objects += 1;
                model.objects.insert(name.clone(), object_def);
            }
        }

        model.stats.timelines = model.timelines.len();
        model.stats.variables = model.variables.len();
        model.stats.frames = model
            .objects
            .values()
            .flat_map(|object| object.motions.values())
            .map(|motion| count_frames(&motion.layers))
            .sum();
        model.raw = value_to_json(root);
        model
    }
}

fn objects_map(value: &PsbValue) -> Vec<(String, &PsbValue)> {
    let mut items: Vec<_> = obj_or_empty(Some(value))
        .iter()
        .map(|(name, value)| (name.to_string(), value))
        .collect();
    items.sort_by(|a, b| a.0.cmp(&b.0));
    items
}

fn screen_size(value: Option<&PsbValue>) -> ScreenSize {
    let map = obj_or_empty(value);
    ScreenSize {
        width: get_f64(map, "width").unwrap_or_default(),
        height: get_f64(map, "height").unwrap_or_default(),
        origin_x: get_f64(map, "originX").unwrap_or_default(),
        origin_y: get_f64(map, "originY").unwrap_or_default(),
    }
}

fn texture_source(name: &str, value: &PsbValue) -> TextureSource {
    let map = obj_or_empty(Some(value));
    let texture = obj_or_empty(get(map, "texture"));
    let mut icons = BTreeMap::new();
    let mut named_icons = BTreeMap::new();
    for (key, icon) in obj_or_empty(get(map, "icon")) {
        let icon = atlas_icon(icon);
        match key.parse::<u32>() {
            Ok(id) => {
                icons.insert(id, icon);
            }
            Err(_) => {
                named_icons.insert(key.to_string(), icon);
            }
        }
    }
    TextureSource {
        name: name.to_string(),
        kind: get_i64(map, "type"),
        width: get_i64(texture, "width").unwrap_or_default().max(0) as u32,
        height: get_i64(texture, "height").unwrap_or_default().max(0) as u32,
        truncated_width: get_i64(texture, "truncated_width")
            .unwrap_or_default()
            .max(0) as u32,
        truncated_height: get_i64(texture, "truncated_height")
            .unwrap_or_default()
            .max(0) as u32,
        format: get_str(texture, "type"),
        resource: get(texture, "pixel").and_then(resource_index),
        icons,
        named_icons,
    }
}

fn resource_index(value: &PsbValue) -> Option<u32> {
    match value {
        PsbValue::Resource(index) => Some(*index),
        _ => as_i64(value).map(|value| value.max(0) as u32),
    }
}

fn atlas_icon(value: &PsbValue) -> AtlasIcon {
    let map = obj_or_empty(Some(value));
    AtlasIcon {
        attr: get_i64(map, "attr"),
        left: get_f64(map, "left").unwrap_or_default(),
        top: get_f64(map, "top").unwrap_or_default(),
        width: get_f64(map, "width").unwrap_or_default(),
        height: get_f64(map, "height").unwrap_or_default(),
        origin_x: get_f64(map, "originX").unwrap_or_default(),
        origin_y: get_f64(map, "originY").unwrap_or_default(),
    }
}

fn variables(metadata: &PsbMap) -> Vec<VariableDef> {
    let mut out = Vec::new();
    for entry in list_or_empty(get(metadata, "variableList")) {
        let map = obj_or_empty(Some(entry));
        let mut states = Vec::new();
        for state in list_or_empty(get(map, "frameList")) {
            let state = obj_or_empty(Some(state));
            states.push(VariableState {
                frame: get_i64(state, "frame").unwrap_or_default(),
                label: get_str(state, "label").unwrap_or_default(),
            });
        }
        out.push(VariableDef {
            label: get_str(map, "label").unwrap_or_default(),
            states,
        });
    }
    out
}

fn timelines(metadata: &PsbMap) -> Vec<Timeline> {
    let mut out = Vec::new();
    for entry in list_or_empty(get(metadata, "timelineControl")) {
        let map = obj_or_empty(Some(entry));
        let mut tracks = Vec::new();
        for track in list_or_empty(get(map, "variableList")) {
            let track = obj_or_empty(Some(track));
            let mut frames = Vec::new();
            for frame in list_or_empty(get(track, "frameList")) {
                let frame = obj_or_empty(Some(frame));
                let content = obj_or_empty(get(frame, "content"));
                frames.push(TimelineFrame {
                    time: get_i64(frame, "time").unwrap_or_default(),
                    kind: get_i64(frame, "type").unwrap_or_default(),
                    easing: get_i64(content, "easing"),
                    value: get_f64(content, "value").unwrap_or_default(),
                });
            }
            tracks.push(TimelineTrack {
                label: get_str(track, "label").unwrap_or_default(),
                frames,
            });
        }
        out.push(Timeline {
            label: get_str(map, "label").unwrap_or_default(),
            last_time: get_i64(map, "lastTime").unwrap_or_default(),
            loop_begin: get_i64(map, "loopBegin").unwrap_or_default(),
            loop_end: get_i64(map, "loopEnd").unwrap_or_default(),
            tracks,
        });
    }
    out
}

fn controllers(metadata: &PsbMap) -> Controllers {
    Controllers {
        bust: controller_list(metadata, "bustControl"),
        hair: controller_list(metadata, "hairControl"),
        parts: controller_list(metadata, "partsControl"),
        eye: controller_list(metadata, "eyeControl"),
        eyebrow: controller_list(metadata, "eyebrowControl"),
        mouth: controller_list(metadata, "mouthControl"),
        timeline: controller_list(metadata, "timelineControl"),
        transition: controller_list(metadata, "transitionControl"),
        clamp: controller_list(metadata, "clampControl"),
        loop_control: controller_list(metadata, "loopControl"),
    }
}

fn controller_list(metadata: &PsbMap, key: &str) -> Vec<Controller> {
    list_or_empty(get(metadata, key))
        .iter()
        .map(|entry| {
            let map = obj_or_empty(Some(entry));
            Controller {
                label: get_str(map, "label"),
                enabled: get_bool(map, "enabled"),
                parameter: get_str(map, "parameter"),
                variable_lr: get_str(map, "var_lr"),
                variable_ud: get_str(map, "var_ud"),
                variable_lrm: get_str(map, "var_lrm"),
                raw: value_to_json(entry),
            }
        })
        .collect()
}

fn list_or_empty(value: Option<&PsbValue>) -> &[PsbValue] {
    static EMPTY: [PsbValue; 0] = [];
    value.and_then(as_list).unwrap_or(&EMPTY)
}

fn motion(value: &PsbValue) -> Motion {
    let map = obj_or_empty(Some(value));
    let bounds = obj_or_empty(get(map, "bounds"));
    let mut layer_index_map = BTreeMap::new();
    for (label, index) in obj_or_empty(get(map, "layerIndexMap")) {
        if let Some(index) = as_i64(index) {
            layer_index_map.insert(label.to_string(), index);
        }
    }
    Motion {
        last_time: get_i64(map, "lastTime").unwrap_or_default(),
        bounds: Bounds {
            left: get_f64(bounds, "left").unwrap_or_default(),
            top: get_f64(bounds, "top").unwrap_or_default(),
            right: get_f64(bounds, "right").unwrap_or_default(),
            bottom: get_f64(bounds, "bottom").unwrap_or_default(),
        },
        layer_index_map,
        layers: layers(get(map, "layer")),
    }
}

fn layers(value: Option<&PsbValue>) -> Vec<Layer> {
    list_or_empty(value).iter().map(layer).collect()
}

fn layer(value: &PsbValue) -> Layer {
    let map = obj_or_empty(Some(value));
    let mut frames = Vec::new();
    for frame in list_or_empty(get(map, "frameList")) {
        let frame = obj_or_empty(Some(frame));
        frames.push(Frame {
            time: get_i64(frame, "time").unwrap_or_default(),
            kind: get_i64(frame, "type").unwrap_or_default(),
            content: get(frame, "content").map(content),
        });
    }
    Layer {
        label: get_str(map, "label").unwrap_or_default(),
        kind: get_i64(map, "type").unwrap_or_default(),
        coordinate: get_i64(map, "coordinate"),
        export_self: get_i64(map, "exportSelf"),
        ground_correction: get_i64(map, "groundCorrection"),
        inherit_mask: get_i64(map, "inheritMask"),
        join_target: get_i64(map, "joinTarget"),
        mesh_combine: get_i64(map, "meshCombine"),
        mesh_transform: get_i64(map, "meshTransform"),
        mesh_sync_child_mask: get_i64(map, "meshSyncChildMask"),
        mesh_division: get_i64(map, "meshDivision"),
        parameterize: get_i64(map, "parameterize"),
        stencil_type: get_i64(map, "stencilType"),
        obj_tri_priority: get_i64(map, "objTriPriority"),
        frames,
        children: layers(get(map, "children")),
    }
}

fn content(value: &PsbValue) -> Content {
    let map = obj_or_empty(Some(value));
    Content {
        src: get_str(map, "src"),
        icon: get(map, "icon").and_then(icon_ref),
        mask: get_i64(map, "mask"),
        coord: get(map, "coord").and_then(vector::<2>),
        angle: get_f64(map, "angle"),
        color: get(map, "color").and_then(vector::<4>),
        opa: get_f64(map, "opa"),
        mesh: get(map, "mesh").map(mesh),
        motion: get_str(map, "motion"),
    }
}

fn icon_ref(value: &PsbValue) -> Option<IconRef> {
    match value {
        PsbValue::String(raw) => {
            let id = raw.parse::<u32>().ok();
            let rect = if id.is_none() { parse_rect(raw) } else { None };
            Some(IconRef {
                raw: raw.to_string(),
                id,
                rect,
            })
        }
        _ => as_i64(value).map(|id| IconRef {
            raw: id.to_string(),
            id: u32::try_from(id).ok(),
            rect: None,
        }),
    }
}

fn parse_rect(raw: &str) -> Option<[f64; 4]> {
    let mut parts = raw.split(':');
    let left = parts.next()?.parse().ok()?;
    let top = parts.next()?.parse().ok()?;
    let width = parts.next()?.parse().ok()?;
    let height = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some([left, top, width, height])
}

fn mesh(value: &PsbValue) -> Mesh {
    let map = obj_or_empty(Some(value));
    Mesh {
        bp: get(map, "bp").map(flat_floats),
        cc: get(map, "cc").map(flat_floats),
    }
}

fn flat_floats(value: &PsbValue) -> Vec<f64> {
    list_or_empty(Some(value))
        .iter()
        .map(|value| as_f64(value).unwrap_or_default())
        .collect()
}

fn vector<const N: usize>(value: &PsbValue) -> Option<[f64; N]> {
    let items = as_list(value)?;
    let mut out = [0.0; N];
    for (index, slot) in out.iter_mut().enumerate() {
        *slot = as_f64(items.get(index)?)?;
    }
    Some(out)
}

fn count_layers(layers: &[Layer]) -> usize {
    layers
        .iter()
        .map(|layer| 1 + count_layers(&layer.children))
        .sum()
}

fn count_frames(layers: &[Layer]) -> usize {
    layers
        .iter()
        .map(|layer| layer.frames.len() + count_frames(&layer.children))
        .sum()
}

impl Model {
    /// Total number of layers including descendants.
    pub fn layer_count(&self) -> usize {
        self.objects
            .values()
            .flat_map(|object| object.motions.values())
            .map(|motion| count_layers(&motion.layers))
            .sum()
    }

    /// Look up a motion by object and motion name.
    pub fn motion(&self, object: &str, motion: &str) -> Option<&Motion> {
        self.objects.get(object)?.motions.get(motion)
    }

    /// Look up a timeline by name.
    pub fn timeline(&self, name: &str) -> Option<&Timeline> {
        self.timelines
            .iter()
            .find(|timeline| timeline.label == name)
    }

    /// Number of texture pages (typed accessor; serialized as `sources`).
    pub fn source_count(&self) -> usize {
        self.sources.len()
    }
}

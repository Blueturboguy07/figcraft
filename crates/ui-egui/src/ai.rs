//! AI tools through publik API or the person's own key (desktop only). The setup flow, the
//! credential file and the HTTP calls live in the `craft-publik` crate, which the Craft apps share;
//! this module is FigCraft's thin host: the first-launch window, Preferences ▸ AI, and
//! Object ▸ AI ▸ Draft copy and Generate image.
//!
//! Nothing is sent, and nothing is charged, until the person clicks Write copy or Generate image in
//! one of the two tool windows. The file changes only through commands (`text.set` for the copy,
//! `node.set` for an image fill, `image.place` for a new image layer), so each change is one undo
//! step.

use std::sync::mpsc::{Receiver, TryRecvError};

pub use craft_publik::AppInfo;
use craft_publik::client::{self, Fail, Reply};
use craft_publik::ui::{Action, AiState, spawn};
use craft_publik::{Line, Provider, WHY_IT_COSTS};
use egui::{Context, CornerRadius, Id, Margin, RichText, Ui};
use figcraft_doc::{Node, NodeId, NodeKind, TextAutoResize};
use serde_json::json;

use crate::dialogs::{AiDialog, window};
use crate::i18n::{tr, trf};
use crate::theme::Tokens;
use crate::{FigApp, UiState, widgets};

const COPY_SYSTEM: &str = "You write copy for a text layer in a user interface design: an app screen, a web page or a component. Reply with the copy only: no quotation marks around it, no markdown, no notes. Put each paragraph on its own line, with no blank lines between paragraphs. Write in the language of the brief unless the brief names another language. Stay within the length limit, so that the copy fits the layer.";

/// The sample that measures how much text a fixed-size text box holds (in its current type style).
const FIT_SAMPLE: &str = "Good copy is short and clear. It says one thing well, and the reader sees why it matters. ";
/// The most text the measure sets; a box that holds more is "more than" this.
const FIT_CAP: usize = 2000;
/// Characters per word for the word estimate (English averages five letters and a space).
const CHARS_PER_WORD: usize = 6;
/// A growing text layer with less text than this gets no length hint from its text.
const SHORT: usize = 12;
/// The tool windows' width.
const WIDTH: f32 = 380.0;
/// The name of a new layer that holds a generated image.
const GENERATED_NAME: &str = "Generated image";

pub struct Ai {
    pub state: AiState,
    frames: u32,
    first_run_checked: bool,
    /// The first-launch window is due; it opens when no other window shows.
    setup_due: bool,
    /// Preferences ▸ AI shows the setup flow in place of the status.
    inline_setup: bool,
    copy: Job,
    /// The text layer that Replace text changes, chosen at the Write copy click.
    copy_target: Option<TextTarget>,
    /// How much text the target layer holds, for one revision of its file.
    fit: Option<(FitKey, Option<Fit>)>,
    image: Job,
    /// Where the generated image goes, chosen at the Generate image click.
    image_target: Option<ImageTarget>,
    shape: Shape,
    /// A generated image that is not on the canvas yet. It was paid for, so it is never dropped.
    unplaced: Option<Vec<u8>>,
}

/// One tool's request in flight and its last answer.
#[derive(Default)]
struct Job {
    prompt: String,
    result: Option<String>,
    error: Option<String>,
    rx: Option<Receiver<Result<Reply, Fail>>>,
}

impl Job {
    fn busy(&self) -> bool {
        self.rx.is_some()
    }

    /// The reply, once it arrived. A failure goes to `error`, or to the notice bar (one message,
    /// one link) when it is a 402 or a key problem.
    fn poll(&mut self, state: &mut AiState) -> Option<Reply> {
        let rx = self.rx.take()?;
        match rx.try_recv() {
            Ok(Ok(r)) => {
                state.note_reply(&r);
                self.error = None;
                Some(r)
            }
            Ok(Err(f)) => {
                state.note_fail(&f);
                let notice = f.status == 402 || f.status == 401 || f.reprovision;
                if !notice {
                    self.error = Some(f.message);
                }
                None
            }
            Err(TryRecvError::Empty) => {
                self.rx = Some(rx);
                None
            }
            Err(TryRecvError::Disconnected) => {
                self.error = Some(tr("The request stopped before it finished. Try again.").to_string());
                None
            }
        }
    }

    fn start(&mut self, rx: Receiver<Result<Reply, Fail>>) {
        self.result = None;
        self.error = None;
        self.rx = Some(rx);
    }
}

/// A text layer in an open file: (file uid, layer).
type TextTarget = (u64, NodeId);
/// The file, layer and file revision a [`Fit`] was measured for.
type FitKey = (u64, NodeId, u64);

/// How much text a text layer takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Fit {
    /// A fixed box: the characters it holds. A growing layer: the characters it has now.
    chars: usize,
    /// The layer grows to fit its text (auto width or auto height).
    grows: bool,
    /// The fixed box holds more than [`FIT_CAP`] characters.
    more: bool,
}

impl Fit {
    fn words(self) -> usize {
        (self.chars / CHARS_PER_WORD).max(1)
    }
}

/// Where a generated image goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ImageTarget {
    /// The file (its uid).
    doc: u64,
    /// The selected frame or rectangle the image fills, or `None` for a new layer.
    layer: Option<NodeId>,
}

/// The image's proportions (the sizes the image line accepts).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    Square,
    Landscape,
    Portrait,
}

impl Shape {
    const ALL: [Shape; 3] = [Shape::Square, Shape::Landscape, Shape::Portrait];

    fn size(self) -> &'static str {
        match self {
            Shape::Square => "1024x1024",
            Shape::Landscape => "1536x1024",
            Shape::Portrait => "1024x1536",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Shape::Square => tr("Square"),
            Shape::Landscape => tr("Landscape"),
            Shape::Portrait => tr("Portrait"),
        }
    }

    /// The shape nearest a layer's proportions.
    fn for_size(width: f64, height: f64) -> Shape {
        if !(width.is_finite() && height.is_finite()) || width <= 0.0 || height <= 0.0 {
            return Shape::Square;
        }
        let ratio = width / height;
        if ratio > 1.2 {
            Shape::Landscape
        } else if ratio < 1.0 / 1.2 {
            Shape::Portrait
        } else {
            Shape::Square
        }
    }
}

impl Ai {
    pub fn new(app: AppInfo) -> Self {
        Ai {
            state: AiState::new(app),
            frames: 0,
            first_run_checked: false,
            setup_due: false,
            inline_setup: false,
            copy: Job::default(),
            copy_target: None,
            fit: None,
            image: Job::default(),
            image_target: None,
            shape: Shape::Square,
            unplaced: None,
        }
    }

    /// No first-launch window in this run (an agent drives the app over `--control`). Object ▸ AI
    /// and Preferences ▸ AI still offer the setup.
    pub fn skip_first_run(&mut self) {
        self.first_run_checked = true;
    }
}

/// `PUBLIK_SELF_TEST=1`: provision for real and print one line (publikhq.com/skills/setup.md).
pub fn self_test(app: &AppInfo) -> String {
    craft_publik::self_test(app)
}

/// Once per frame of the main window, before it draws: finish background work, put an image that
/// arrived on the canvas, and open the first-launch window when it is due.
pub fn frame(app: &mut FigApp) {
    let Some(ai) = app.ai.as_mut() else { return };
    ai.state.poll();
    if let Some(r) = ai.copy.poll(&mut ai.state) {
        ai.copy.result = Some(clean_copy(&r.text));
    }
    let image = ai.image.poll(&mut ai.state).map(|r| (r.bytes, ai.image_target.take()));
    ai.frames = ai.frames.saturating_add(1);
    // The first launch asks once the window is up (a few frames in). "Later" ends that for good.
    if !ai.first_run_checked && ai.frames > 3 {
        ai.first_run_checked = true;
        ai.setup_due = ai.state.wants_first_run() || ai.state.owes_first_card();
    }
    if ai.setup_due && !other_window_open(&app.ui) {
        ai.setup_due = false;
        ai.state.begin();
        app.ui.ai_dialog = Some(AiDialog::Setup);
    }
    if let Some((bytes, target)) = image {
        place_generated(app, bytes, target);
    }
}

/// Another window or a full-window view is up: the first-launch window waits for it to close.
fn other_window_open(u: &UiState) -> bool {
    u.ai_dialog.is_some()
        || u.about
        || u.preferences
        || u.confirm_close
        || u.export_preview
        || u.plugins_window
        || u.menu_input.is_some()
        || u.find.is_some()
        || u.batch_rename.is_some()
        || u.import_report.is_some()
        || u.missing_fonts.is_some()
        || u.color_profile.is_some()
        || u.variables.is_some()
        || u.present.is_some()
        || u.palette.is_some()
}

/// Object ▸ AI ▸ Draft copy… and Generate image…: open the tool's window. Nothing is sent here.
pub fn open(app: &mut FigApp, dialog: AiDialog) {
    if dialog == AiDialog::GenerateImage {
        let shape = image_target(app).and_then(|t| t.layer).and_then(|id| layer_size(app, id)).map(|(w, h)| Shape::for_size(w, h));
        if let Some(ai) = app.ai.as_mut()
            && !ai.image.busy()
        {
            ai.shape = shape.unwrap_or(Shape::Square);
            ai.image.result = None;
            ai.image.error = None;
        }
    }
    app.ui.ai_dialog = Some(dialog);
}

/// An AI window closed with its close button or Escape. Closing the first-launch window counts as
/// Later. A request in flight goes on: its answer waits in the tool's window.
pub fn dismissed(app: &mut FigApp, dialog: AiDialog) {
    if dialog != AiDialog::Setup {
        return;
    }
    let Some(ai) = app.ai.as_mut() else { return };
    if ai.state.settings.setup_done || ai.state.busy() {
        return;
    }
    ai.state.settings.setup_done = true;
    let path = ai.state.app.settings_path();
    if let Err(e) = ai.state.settings.save(path.as_deref()) {
        app.toast(trf("Could not save the AI settings: {error}", &[("error", &e)]));
    }
}

/// The AI window that is open, if any. Called with the other dialogs.
pub fn show(app: &mut FigApp, ctx: &Context) {
    match app.ui.ai_dialog {
        Some(AiDialog::Setup) => setup_window(app, ctx),
        Some(AiDialog::DraftCopy) => draft_copy_window(app, ctx),
        Some(AiDialog::GenerateImage) => generate_image_window(app, ctx),
        None => {}
    }
}

/// Do what the shared setup flow asks; `true` when it asks to close.
fn act(app: &mut FigApp, action: Action) -> bool {
    match action {
        Action::None => false,
        Action::OpenUrl(url) => {
            app.services.open_url(&url);
            false
        }
        Action::Close => true,
    }
}

/// The body of an AI window in a run without AI (no folder for settings).
fn off(ui: &mut Ui, t: &Tokens) {
    ui.label(RichText::new(tr("AI tools are off: FigCraft has no folder for its settings on this computer.")).color(t.text_secondary));
}

/// A tool window's body while AI is not set up. `true` when Set up AI… was clicked.
fn not_ready(ui: &mut Ui) -> bool {
    ui.label(tr("This tool needs AI. Set it up first: publik API (no key to paste) or your own key."));
    ui.add_space(8.0);
    widgets::button(ui, tr("Set up AI…"), true, true).clicked()
}

/// Turn a tool window into the setup flow.
fn to_setup(app: &mut FigApp) {
    if let Some(ai) = app.ai.as_mut() {
        ai.state.begin();
    }
    app.ui.ai_dialog = Some(AiDialog::Setup);
}

fn error_line(ui: &mut Ui, t: &Tokens, error: Option<&str>) {
    if let Some(e) = error {
        ui.add_space(4.0);
        ui.label(RichText::new(e).color(t.danger));
    }
}

fn prompt_field(ui: &mut Ui, text: &mut String, enabled: bool, hint: &str) {
    ui.add_enabled(enabled, egui::TextEdit::multiline(text).desired_rows(3).desired_width(f32::INFINITY).hint_text(hint));
}

// ---- the setup flow (first launch) ----------------------------------------------------------------

fn setup_window(app: &mut FigApp, ctx: &Context) {
    let t = Tokens::get(ctx);
    let mut open = true;
    let mut action = Action::None;
    window(tr("AI"), ctx).id(Id::new("ai_setup")).open(&mut open).show(ctx, |ui| {
        ui.set_min_width(WIDTH);
        match app.ai.as_mut() {
            Some(ai) => action = ai.state.setup_ui(ui),
            None => off(ui, &t),
        }
    });
    let close = act(app, action);
    if !open {
        app.ui.ai_dialog = None;
        dismissed(app, AiDialog::Setup);
    } else if close {
        app.ui.ai_dialog = None;
    }
}

// ---- Draft copy --------------------------------------------------------------------------------------

/// The text layer Draft copy writes for: the text being edited, else the one selected text layer.
fn copy_target(app: &FigApp) -> Option<TextTarget> {
    let ds = app.session.ds().ok()?;
    let id = match (&ds.text_edit, ds.selection.as_slice()) {
        (Some(edit), _) => edit.id,
        (None, [id]) => *id,
        _ => return None,
    };
    ds.doc.node(id)?.kind.text()?;
    Some((ds.uid, id))
}

/// How much text layer `n` takes: what its fixed box holds in its current type style (measured on
/// a copy of its text), or, for a layer that grows, how much text it has now.
fn measure_fit(n: &Node) -> Option<Fit> {
    let t = n.kind.text()?;
    match t.auto_resize {
        TextAutoResize::WidthAndHeight | TextAutoResize::Height => Some(Fit { chars: t.characters.chars().count(), grows: true, more: false }),
        TextAutoResize::None | TextAutoResize::Truncate => {
            let (w, h) = (n.width, n.height);
            if !(w.is_finite() && h.is_finite()) || w <= 0.0 || h <= 0.0 {
                return None;
            }
            let sample: String = FIT_SAMPLE.chars().cycle().take(FIT_CAP).collect();
            let fits = |k: usize| {
                let mut s = t.clone();
                s.characters = sample.chars().take(k).collect();
                s.runs.clear();
                s.paragraphs.clear();
                s.max_lines = 0;
                s.auto_resize = TextAutoResize::Height;
                figcraft_text::layout(&s, Some(w), None).height <= h + 0.5
            };
            if fits(FIT_CAP) {
                return Some(Fit { chars: FIT_CAP, grows: false, more: true });
            }
            // The most that fits, to within a few characters.
            let (mut lo, mut hi) = (0, FIT_CAP);
            while hi - lo > 4 {
                let mid = lo + (hi - lo) / 2;
                if fits(mid) {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            Some(Fit { chars: lo, grows: false, more: false })
        }
    }
}

/// [`measure_fit`] for a layer of the file in view, kept for the file's revision.
fn fit_of(app: &mut FigApp, (uid, id): TextTarget) -> Option<Fit> {
    let ds = app.session.ds().ok().filter(|d| d.uid == uid)?;
    let key = (uid, id, ds.revision);
    if let Some((k, fit)) = app.ai.as_ref().and_then(|ai| ai.fit)
        && k == key
    {
        return fit;
    }
    let fit = ds.doc.node(id).and_then(measure_fit);
    if let Some(ai) = app.ai.as_mut() {
        ai.fit = Some((key, fit));
    }
    fit
}

/// The copy from a chat answer: no code fence, no quotation marks around it, and one line per
/// paragraph (a blank line between paragraphs would be an empty paragraph).
fn clean_copy(text: &str) -> String {
    let text = text.replace("\r\n", "\n");
    let lines: Vec<&str> = text.lines().map(str::trim_end).filter(|l| !l.trim_start().starts_with("```")).filter(|l| !l.trim().is_empty()).collect();
    let joined = lines.join("\n");
    let trimmed = joined.trim();
    for (open, close) in [('"', '"'), ('\u{201C}', '\u{201D}'), ('\u{00AB}', '\u{00BB}')] {
        if let Some(inner) = trimmed.strip_prefix(open).and_then(|t| t.strip_suffix(close))
            && !inner.contains(open)
            && !inner.contains(close)
        {
            return inner.trim().to_string();
        }
    }
    trimmed.to_string()
}

/// The request for the model: the brief and the length the layer takes.
fn copy_request(brief: &str, fit: Option<Fit>) -> String {
    let limit = match fit {
        Some(f) if f.more => format!("The text box holds more than {} characters. Keep to the length the brief asks for.", f.chars),
        Some(f) if f.grows && f.chars >= SHORT => format!(
            "The text layer grows to fit its text, which is now about {} words ({} characters). Keep to a similar length unless the brief asks for another length.",
            f.words(),
            f.chars
        ),
        Some(f) if f.grows => "The text layer grows to fit its text. Keep to the length the brief asks for, or a few words.".to_string(),
        Some(f) => format!("Length limit: about {} words ({} characters), so that the copy fits the text box.", f.words(), f.chars),
        None => "No text layer is chosen. Keep to the length the brief asks for, or about 40 words.".to_string(),
    };
    format!("Brief: {}\n{limit}", brief.trim())
}

/// What the Draft copy window asks for after it drew.
#[derive(Default)]
struct CopyOut {
    setup: bool,
    replace: Option<(TextTarget, String)>,
    copy: Option<String>,
    action: Option<Action>,
}

fn draft_copy_window(app: &mut FigApp, ctx: &Context) {
    let t = Tokens::get(ctx);
    let target = copy_target(app);
    let fit = target.and_then(|tg| fit_of(app, tg));
    let mut open = true;
    let mut out = CopyOut::default();
    window(tr("Draft copy"), ctx).id(Id::new("ai_draft_copy")).open(&mut open).show(ctx, |ui| {
        ui.set_width(WIDTH);
        let Some(ai) = app.ai.as_mut() else {
            off(ui, &t);
            return;
        };
        let provider = ai.state.provider();
        if !provider.is_set_up() {
            out.setup = not_ready(ui);
            return;
        }
        let note = match (target, fit) {
            (Some(_), Some(f)) if f.more => trf("For the selected text layer. Its box holds more than {chars} characters.", &[("chars", &f.chars)]),
            (Some(_), Some(f)) if f.grows => {
                trf("For the selected text layer. It grows to fit its text, which is now {chars} characters long.", &[("chars", &f.chars)])
            }
            (Some(_), Some(f)) => trf(
                "For the selected text layer. Its box holds about {words} words ({chars} characters) in its current type style.",
                &[("words", &f.words()), ("chars", &f.chars)],
            ),
            (Some(_), None) => tr("For the selected text layer.").to_string(),
            (None, _) => tr("No text layer is selected. Select one text layer to replace its text, or write copy and copy it.").to_string(),
        };
        ui.label(RichText::new(note).color(t.text_secondary));
        ui.add_space(8.0);
        ui.label(tr("Brief"));
        let busy = ai.copy.busy();
        prompt_field(ui, &mut ai.copy.prompt, !busy, tr("A headline and one line under it for a sign-up screen. Friendly tone."));
        ui.add_space(8.0);
        let mut start = false;
        ui.horizontal(|ui| {
            if busy {
                ui.spinner();
                ui.label(tr("Writing…"));
            } else if widgets::button(ui, tr("Write copy"), true, !ai.copy.prompt.trim().is_empty()).clicked() {
                start = true;
            }
        });
        error_line(ui, &t, ai.copy.error.as_deref());
        if let Some(copy) = ai.copy.result.clone() {
            ui.add_space(8.0);
            egui::Frame::NONE.fill(t.field).corner_radius(CornerRadius::same(5)).inner_margin(Margin::same(8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                egui::ScrollArea::vertical().id_salt("ai_copy_result").max_height(180.0).show(ui, |ui| {
                    ui.add(egui::Label::new(&copy).selectable(true));
                });
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if let Some(tg) = ai.copy_target
                    && widgets::button(ui, tr("Replace text"), true, !busy).clicked()
                {
                    out.replace = Some((tg, copy.clone()));
                }
                if widgets::button(ui, tr("Copy"), false, true).clicked() {
                    out.copy = Some(copy.clone());
                }
            });
        }
        ui.add_space(6.0);
        out.action = Some(ai.state.result_bar(ui));
        if start {
            ai.copy_target = target;
            let user = copy_request(&ai.copy.prompt, fit);
            ai.copy.start(spawn(ui.ctx(), move || client::chat(&provider, COPY_SYSTEM, &user)));
        }
    });
    if out.setup {
        to_setup(app);
        return;
    }
    let mut close = out.action.is_some_and(|a| act(app, a));
    if let Some(text) = out.copy {
        app.services.clipboard_set_text(&text);
        app.toast(tr("Copied"));
    }
    if let Some((tg, copy)) = out.replace {
        match replace_text(app, tg, &copy) {
            Ok(()) => {
                app.toast(tr("Replaced the text of the layer"));
                close = true;
            }
            Err(e) => {
                if let Some(ai) = app.ai.as_mut() {
                    ai.copy.error = Some(e);
                }
            }
        }
    }
    if !open || close {
        app.ui.ai_dialog = None;
    }
}

/// Replace text: the layer's whole text becomes `copy` (one undo step).
fn replace_text(app: &mut FigApp, (uid, id): TextTarget, copy: &str) -> Result<(), String> {
    let ds = app.session.ds().ok().filter(|d| d.uid == uid && d.doc.node(id).is_some_and(|n| n.kind.text().is_some()));
    let Some(ds) = ds else {
        return Err(tr("The text layer is not in the file in view. Open its file, or write copy again for another layer.").to_string());
    };
    // Text being edited on the canvas is committed first: its caret would point into the old text.
    if ds.text_edit.is_some() {
        let _ = app.session.execute("text.edit.end", &json!({}));
    }
    let r = app.session.execute("text.set", &json!({"id": id, "characters": copy}));
    app.handle_ui_requests();
    r.map(|_| ()).map_err(|e| e.to_string())
}

// ---- Generate image -----------------------------------------------------------------------------------

/// A layer an image can fill: a frame (or component, instance) or a rectangle.
fn fillable(kind: &NodeKind) -> bool {
    kind.is_frame_like() || matches!(kind, NodeKind::Rectangle)
}

/// Where an image generated now would go: the one selected frame or rectangle, else a new layer in
/// the file in view. `None` with no file open.
fn image_target(app: &FigApp) -> Option<ImageTarget> {
    let ds = app.session.ds().ok()?;
    let layer = match ds.selection.as_slice() {
        [id] => ds.doc.node(*id).filter(|n| fillable(&n.kind)).map(|_| *id),
        _ => None,
    };
    Some(ImageTarget { doc: ds.uid, layer })
}

fn layer_size(app: &FigApp, id: NodeId) -> Option<(f64, f64)> {
    let n = app.session.doc().ok()?.node(id)?;
    Some((n.width, n.height))
}

/// The pixel size of encoded image `bytes`.
fn image_size(bytes: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?.into_dimensions().ok()
}

/// Put a generated image in its file (which may not be the one in view now: the view stays as it
/// is), else in the file in view. Returns the note that says where it went.
fn place(app: &mut FigApp, bytes: &[u8], target: Option<ImageTarget>) -> Result<&'static str, String> {
    let wanted = target.and_then(|t| Some((app.session.docs.iter().position(|d| d.uid == t.doc)?, t.layer)));
    let (index, layer) = wanted.unwrap_or((app.session.active, None));
    if app.session.docs.get(index).is_none() {
        return Err(tr("Open a file, then click Place image.").to_string());
    }
    let shown = app.session.active;
    app.session.active = index;
    let r = place_in_active(app, bytes, layer);
    app.session.active = shown;
    app.handle_ui_requests();
    r
}

/// [`place`] in the active file: the fill of `layer` when it is still a frame or rectangle, else a
/// new layer the image's size in the middle of the view.
fn place_in_active(app: &mut FigApp, bytes: &[u8], layer: Option<NodeId>) -> Result<&'static str, String> {
    let ds = app.session.ds().map_err(|e| e.to_string())?;
    if ds.preview.is_some() {
        return Err(figcraft_engine::PREVIEW_READ_ONLY.to_string());
    }
    let layer = layer.filter(|id| ds.doc.node(*id).is_some_and(|n| fillable(&n.kind)));
    let (cx, cy) = (ds.view.cx, ds.view.cy);
    match layer {
        Some(id) => {
            let doc = app.session.doc_mut().map_err(|e| e.to_string())?;
            let (hash, _, _) = figcraft_format::add_image(doc, bytes.to_vec()).map_err(|e| e.to_string())?;
            let paint = figcraft_color::Paint::image(hash, figcraft_color::ImageScaleMode::Fill);
            let paint = serde_json::to_value(&paint).map_err(|e| e.to_string())?;
            app.session.execute("node.set", &json!({"ids": [id], "fills": [paint]})).map_err(|e| e.to_string())?;
            Ok(tr("The image is the fill of the layer. Undo removes it."))
        }
        None => {
            let (w, h) = image_size(bytes).ok_or_else(|| tr("The service sent an image that FigCraft cannot read.").to_string())?;
            let (w, h) = (f64::from(w), f64::from(h));
            let (x, y) = ((cx - w / 2.0).round(), (cy - h / 2.0).round());
            let params = json!({"data": client::base64(bytes), "x": x, "y": y, "width": w, "height": h, "name": GENERATED_NAME});
            app.session.execute("image.place", &params).map_err(|e| e.to_string())?;
            Ok(tr("The image is a new layer in the middle of the view. Undo removes it."))
        }
    }
}

fn place_generated(app: &mut FigApp, bytes: Vec<u8>, target: Option<ImageTarget>) {
    match place(app, &bytes, target) {
        Ok(note) => {
            app.toast(note);
            if let Some(ai) = app.ai.as_mut() {
                ai.unplaced = None;
                ai.image.error = None;
                ai.image.result = Some(note.to_string());
            }
        }
        Err(e) => {
            if let Some(ai) = app.ai.as_mut() {
                ai.unplaced = Some(bytes);
                ai.image.error = Some(trf("The image is ready, but FigCraft could not place it: {error}", &[("error", &e)]));
            }
        }
    }
}

/// The cost sentence shown before an image request: the crate's (from publik, or its own), or who
/// bills an own key.
fn cost_line(state: &AiState, provider: &Provider) -> String {
    match provider {
        Provider::Publik { .. } => match &state.credential {
            Some(c) if !c.cost_sentence.trim().is_empty() => c.cost_sentence.clone(),
            _ => WHY_IT_COSTS.to_string(),
        },
        Provider::Own { service, .. } => trf("Your own key's service ({service}) bills this request, not publik.", &[("service", &service.name())]),
        Provider::NotSetUp => String::new(),
    }
}

/// What the Generate image window asks for after it drew.
#[derive(Default)]
struct ImageOut {
    setup: bool,
    place_again: bool,
    action: Option<Action>,
}

fn generate_image_window(app: &mut FigApp, ctx: &Context) {
    let t = Tokens::get(ctx);
    let target = image_target(app);
    let mut open = true;
    let mut out = ImageOut::default();
    window(tr("Generate image"), ctx).id(Id::new("ai_generate_image")).open(&mut open).show(ctx, |ui| {
        ui.set_width(WIDTH);
        let Some(ai) = app.ai.as_mut() else {
            off(ui, &t);
            return;
        };
        let provider = ai.state.provider();
        if !provider.is_set_up() {
            out.setup = not_ready(ui);
            return;
        }
        let images = provider.supports(Line::Images);
        let note = match target {
            Some(ImageTarget { layer: Some(_), .. }) => tr("The image becomes the fill of the selected layer."),
            Some(_) => tr("No frame or rectangle is selected: the image becomes a new layer in the middle of the view."),
            None => tr("Open a file first: the image goes on its canvas."),
        };
        ui.label(RichText::new(note).color(t.text_secondary));
        ui.add_space(8.0);
        ui.label(tr("Describe the image"));
        let busy = ai.image.busy();
        prompt_field(ui, &mut ai.image.prompt, !busy, tr("A flat illustration of a mountain at dawn, soft colors, no text"));
        ui.add_space(8.0);
        ui.label(tr("Shape"));
        let labels: Vec<&str> = Shape::ALL.iter().map(|s| s.label()).collect();
        let active = Shape::ALL.iter().position(|s| *s == ai.shape).unwrap_or(0);
        if let Some(i) = widgets::segmented_text(ui, &labels, active)
            && !busy
            && let Some(s) = Shape::ALL.get(i)
        {
            ai.shape = *s;
        }
        ui.add_space(8.0);
        if images {
            let cost = cost_line(&ai.state, &provider);
            if !cost.is_empty() {
                ui.label(RichText::new(cost).color(t.text_secondary));
            }
        } else {
            ui.label(RichText::new(Fail::unsupported(Line::Images).message).color(t.warning));
        }
        ui.add_space(8.0);
        let mut start = false;
        ui.horizontal(|ui| {
            if busy {
                ui.spinner();
                ui.label(tr("Generating… This can take a minute."));
            } else {
                let can = images && target.is_some() && !ai.image.prompt.trim().is_empty();
                if widgets::button(ui, tr("Generate image"), true, can).clicked() {
                    start = true;
                }
                if ai.unplaced.is_some() && widgets::button(ui, tr("Place image"), false, target.is_some()).clicked() {
                    out.place_again = true;
                }
            }
        });
        error_line(ui, &t, ai.image.error.as_deref());
        if let Some(note) = &ai.image.result {
            ui.add_space(4.0);
            ui.label(note);
        }
        ui.add_space(6.0);
        out.action = Some(ai.state.result_bar(ui));
        if start {
            ai.image_target = target;
            ai.unplaced = None;
            let prompt = ai.image.prompt.trim().to_string();
            let size = ai.shape.size();
            ai.image.start(spawn(ui.ctx(), move || client::image_generate(&provider, &prompt, size)));
        }
    });
    if out.setup {
        to_setup(app);
        return;
    }
    let close = out.action.is_some_and(|a| act(app, a));
    if out.place_again
        && let Some(bytes) = app.ai.as_mut().and_then(|ai| ai.unplaced.take())
    {
        // Into the file in view now: the first target is what failed.
        place_generated(app, bytes, None);
    }
    if !open || close {
        app.ui.ai_dialog = None;
    }
}

// ---- Preferences ▸ AI ------------------------------------------------------------------------------

/// The AI section of Preferences: status, sign-in and key controls, and the setup flow in place.
/// AI settings take effect at once.
pub fn options_section(app: &mut FigApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let Some(ai) = app.ai.as_mut() else {
        off(ui, &t);
        return;
    };
    ui.label(
        RichText::new(tr("Object ▸ AI ▸ Draft copy and Generate image use publik API or your own key. Changes here take effect at once."))
            .color(t.text_secondary),
    );
    ui.add_space(6.0);
    let action = if ai.inline_setup {
        let action = ai.state.setup_ui(ui);
        if action == Action::Close {
            ai.inline_setup = false;
        }
        action
    } else {
        let mut open_setup = false;
        let action = ai.state.options_ui(ui, &mut open_setup);
        ai.inline_setup = open_setup;
        action
    };
    act(app, action);
}

#[cfg(test)]
#[path = "ai_tests.rs"]
mod tests;

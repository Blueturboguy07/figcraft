//! Object ▸ AI, driven through egui headless and the engine: the windows open from the menu and send
//! nothing on their own, closing the first-launch window counts as Later, Draft copy measures the
//! selected text layer and Replace text is one undo step, and a generated image fills the selected
//! frame or rectangle or becomes a new layer the image's size, in its own file.

use egui::accesskit::Role;
use egui::{Event, Key, Modifiers};
use figcraft_doc::NodeId;
use figcraft_engine::Session;
use serde_json::{Value, json};

use super::*;
use crate::dialogs::AiDialog;
use crate::layers_drag_tests::Ui;

fn id(v: Value) -> NodeId {
    NodeId(v["id"].as_u64().unwrap())
}

/// A `w`×`h` PNG.
fn png(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(w, h, image::Rgba([40, 120, 200, 255]));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

/// AI with a placeholder own key, so the tool windows draw their tools. Nothing that sends is
/// clicked, and no settings are written (no settings folder).
fn with_ai(u: &mut Ui) {
    let info = AppInfo {
        slug: "figcraft-ui-test",
        name: "FigCraft",
        version: "0",
        app_token: "",
        dialects: &["chat_completions", "images"],
        config_dir: None,
    };
    let mut ai = Ai::new(info);
    ai.state.settings.provider = "own".into();
    ai.state.settings.own_key = "sk-placeholder".into();
    u.app.ai = Some(ai);
}

fn ai(u: &Ui) -> &Ai {
    u.app.ai.as_ref().unwrap()
}

/// Every label, value and name a screen reader gets now.
fn texts(u: &mut Ui) -> Vec<(Role, String)> {
    u.ctx.enable_accesskit();
    u.frame(vec![]);
    let out = u.frame_output(vec![]);
    let nodes = out.platform_output.accesskit_update.unwrap().nodes;
    nodes
        .into_iter()
        .flat_map(|(_, n)| [n.label().map(|l| (n.role(), l.to_string())), n.value().map(|v| (n.role(), v.to_string()))])
        .flatten()
        .collect()
}

fn shows(texts: &[(Role, String)], text: &str) -> bool {
    texts.iter().any(|(_, t)| t.contains(text))
}

fn escape(u: &mut Ui) {
    u.frame(vec![
        Event::Key { key: Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE },
        Event::Key { key: Key::Escape, physical_key: None, pressed: false, repeat: false, modifiers: Modifiers::NONE },
    ]);
}

#[test]
fn copy_comes_out_clean() {
    assert_eq!(clean_copy("```\nGet started\n\nIt takes a minute.\n```"), "Get started\nIt takes a minute.");
    assert_eq!(clean_copy("\u{201C}Fresh bread daily.\u{201D}"), "Fresh bread daily.");
    assert_eq!(clean_copy("\"One\" and \"two\""), "\"One\" and \"two\"");
    assert_eq!(clean_copy("a\r\n\r\nb  \n"), "a\nb");
    assert_eq!(clean_copy(""), "");
}

#[test]
fn shapes_follow_the_layer() {
    assert_eq!(Shape::for_size(300.0, 100.0), Shape::Landscape);
    assert_eq!(Shape::for_size(100.0, 300.0), Shape::Portrait);
    assert_eq!(Shape::for_size(100.0, 110.0), Shape::Square);
    assert_eq!(Shape::for_size(0.0, 100.0), Shape::Square);
    assert_eq!(Shape::for_size(f64::NAN, 100.0), Shape::Square);
    assert_eq!(Shape::Landscape.size(), "1536x1024");
}

#[test]
fn the_request_names_the_length_limit() {
    assert!(copy_request(" sign up ", Some(Fit { chars: 600, grows: false, more: false })).contains("about 100 words (600 characters)"));
    assert!(copy_request("x", Some(Fit { chars: FIT_CAP, grows: false, more: true })).contains("more than"));
    assert!(copy_request("x", Some(Fit { chars: 60, grows: true, more: false })).contains("similar length"));
    assert!(copy_request("x", Some(Fit { chars: 4, grows: true, more: false })).contains("a few words"));
    assert!(copy_request("sign up", None).starts_with("Brief: sign up\n"));
}

#[test]
fn draft_copy_measures_the_selected_text_layer_and_replaces_its_text() {
    let mut s = Session::new();
    let small = id(s.execute("text.create", &json!({"text": "Old", "x": 0, "y": 0, "width": 160})).unwrap());
    s.execute("node.set", &json!({"ids": [small], "textAutoResize": "none", "height": 40})).unwrap();
    let big = id(s.execute("text.create", &json!({"text": "Old", "x": 0, "y": 100, "width": 400})).unwrap());
    s.execute("node.set", &json!({"ids": [big], "textAutoResize": "none", "height": 300})).unwrap();
    let grows = id(s.execute("text.create", &json!({"text": "Sign up in a minute", "x": 0, "y": 500})).unwrap());
    let rect = id(s.execute("shape.rectangle", &json!({"x": 500, "y": 0, "width": 100, "height": 100})).unwrap());
    let mut u = Ui::new(s);
    with_ai(&mut u);

    // One text layer selected: it is the target, and a bigger fixed box holds more.
    u.app.session.set_selection(vec![small]);
    let target = copy_target(&u.app).unwrap();
    assert_eq!(target.1, small);
    let fit_small = fit_of(&mut u.app, target).unwrap();
    assert!(!fit_small.grows && !fit_small.more && fit_small.chars > 0 && fit_small.chars < FIT_CAP, "{fit_small:?}");
    u.app.session.set_selection(vec![big]);
    let target = copy_target(&u.app).unwrap();
    let fit_big = fit_of(&mut u.app, target).unwrap();
    assert!(fit_big.chars > fit_small.chars, "{fit_big:?} vs {fit_small:?}");
    // A layer that grows reports the text it has now.
    u.app.session.set_selection(vec![grows]);
    let target = copy_target(&u.app).unwrap();
    assert_eq!(fit_of(&mut u.app, target), Some(Fit { chars: 19, grows: true, more: false }));
    // Anything else is no target.
    u.app.session.set_selection(vec![rect]);
    assert_eq!(copy_target(&u.app), None);
    u.app.session.set_selection(vec![small, big]);
    assert_eq!(copy_target(&u.app), None);

    // The measure left the file alone; Replace text is one undoable command.
    let text = |u: &Ui, n: NodeId| u.app.session.doc().unwrap().node(n).unwrap().kind.text().unwrap().characters.clone();
    assert_eq!(text(&u, small), "Old");
    let uid = u.app.session.ds().unwrap().uid;
    replace_text(&mut u.app, (uid, small), "Get started").unwrap();
    assert_eq!(text(&u, small), "Get started");
    u.app.run("edit.undo", json!({}));
    assert_eq!(text(&u, small), "Old");
    assert!(replace_text(&mut u.app, (uid ^ 1, small), "x").is_err(), "another file");
    assert!(replace_text(&mut u.app, (uid, rect), "x").is_err(), "not a text layer");
}

#[test]
fn a_generated_image_fills_the_selected_frame_or_rectangle_or_becomes_a_new_layer() {
    let mut s = Session::new();
    let frame = id(s.execute("frame.create", &json!({"x": 0, "y": 0, "width": 300, "height": 100})).unwrap());
    let rect = id(s.execute("shape.rectangle", &json!({"x": 400, "y": 0, "width": 50, "height": 150})).unwrap());
    let text = id(s.execute("text.create", &json!({"text": "Keep", "x": 0, "y": 300})).unwrap());
    let mut u = Ui::new(s);
    with_ai(&mut u);
    let fill = |u: &Ui, n: NodeId| u.app.session.doc().unwrap().node(n).unwrap().fills.clone();
    let is_image = |p: &figcraft_color::Paint| matches!(p.kind, figcraft_color::PaintKind::Image(_));

    // A frame selected: the image is its only fill, and its proportions pick the shape.
    u.app.session.set_selection(vec![frame]);
    let target = image_target(&u.app).unwrap();
    assert_eq!(target.layer, Some(frame));
    open(&mut u.app, AiDialog::GenerateImage);
    assert_eq!(ai(&u).shape, Shape::Landscape);
    assert!(place(&mut u.app, &png(16, 8), Some(target)).is_ok());
    let fills = fill(&u, frame);
    assert!(fills.len() == 1 && is_image(&fills[0]), "{fills:?}");
    // One undo step puts the old fill back.
    u.app.run("edit.undo", json!({}));
    assert!(!fill(&u, frame).iter().any(is_image));

    // A rectangle selected: the same, and a tall one asks for a portrait image.
    u.app.session.set_selection(vec![rect]);
    let target = image_target(&u.app).unwrap();
    open(&mut u.app, AiDialog::GenerateImage);
    assert_eq!(ai(&u).shape, Shape::Portrait);
    place(&mut u.app, &png(8, 16), Some(target)).unwrap();
    assert!(fill(&u, rect).iter().any(is_image));

    // A text layer keeps its text: the image becomes a new layer the image's size, in the middle of
    // the view, and it is selected.
    u.app.session.set_selection(vec![text]);
    let target = image_target(&u.app).unwrap();
    assert_eq!(target.layer, None);
    let layers = |u: &Ui| u.app.session.doc().unwrap().node(u.app.session.page().unwrap()).unwrap().children.len();
    let before = layers(&u);
    place(&mut u.app, &png(64, 32), Some(target)).unwrap();
    assert_eq!(layers(&u), before + 1);
    let new = *u.app.session.selection().first().unwrap();
    let n = u.app.session.doc().unwrap().node(new).unwrap();
    assert_eq!((n.width, n.height, n.name.as_str()), (64.0, 32.0, GENERATED_NAME));
    assert!(n.fills.iter().any(is_image));
}

#[test]
fn an_image_goes_into_its_own_file_and_waits_without_one() {
    let mut s = Session::new();
    let frame = id(s.execute("frame.create", &json!({"x": 0, "y": 0, "width": 100, "height": 100})).unwrap());
    s.set_selection(vec![frame]);
    let mut u = Ui::new(s);
    with_ai(&mut u);
    let target = image_target(&u.app).unwrap();
    // Another file comes into view while the image is on its way.
    u.app.run("file.new", json!({}));
    let shown = u.app.session.ds().unwrap().uid;
    assert_ne!(shown, target.doc);
    place_generated(&mut u.app, png(4, 4), Some(target));
    assert_eq!(u.app.session.ds().unwrap().uid, shown, "the view stays on the file in view");
    let first = u.app.session.docs.iter().find(|d| d.uid == target.doc).unwrap();
    assert!(first.doc.node(frame).unwrap().fills.iter().any(|p| matches!(p.kind, figcraft_color::PaintKind::Image(_))));
    assert!(ai(&u).unplaced.is_none() && ai(&u).image.result.is_some());

    // With no file open, the image waits for Place image: it was paid for.
    let mut u = Ui::new(Session::empty());
    with_ai(&mut u);
    assert!(image_target(&u.app).is_none());
    place_generated(&mut u.app, png(4, 4), None);
    assert!(ai(&u).unplaced.is_some() && ai(&u).image.error.is_some());
}

#[test]
fn the_ai_windows_open_from_the_menu_and_send_nothing_on_their_own() {
    let mut s = Session::new();
    let text = id(s.execute("text.create", &json!({"text": "Old", "x": 40, "y": 40, "width": 200})).unwrap());
    s.set_selection(vec![text]);
    let mut u = Ui::new(s);
    with_ai(&mut u);

    // Draft copy: where the copy goes, the brief and Write copy; Escape closes it, nothing sent.
    crate::menus::invoke(&mut u.app, "ai.draftCopy");
    assert_eq!(u.app.ui.ai_dialog, Some(AiDialog::DraftCopy));
    let shown = texts(&mut u);
    assert!(shows(&shown, "For the selected text layer."), "{shown:?}");
    assert!(shown.iter().any(|(r, t)| *r == Role::Button && t == "Write copy"), "{shown:?}");
    escape(&mut u);
    assert_eq!(u.app.ui.ai_dialog, None);
    assert!(!ai(&u).copy.busy());

    // Generate image: where the image goes, the shapes and who bills the request, before any send.
    crate::menus::invoke(&mut u.app, "ai.generateImage");
    assert_eq!(u.app.ui.ai_dialog, Some(AiDialog::GenerateImage));
    let shown = texts(&mut u);
    for expected in ["No frame or rectangle is selected", "Landscape", "Your own key's service (OpenAI) bills this request", "Generate image"] {
        assert!(shows(&shown, expected), "{expected}: {shown:?}");
    }
    escape(&mut u);
    assert_eq!(u.app.ui.ai_dialog, None);
    assert!(!ai(&u).image.busy() && ai(&u).state.notice.is_none());

    // The menu lists both under Object ▸ AI, and they are available in the desktop app.
    assert!(crate::menus::available("ai.draftCopy") && crate::menus::available("ai.generateImage"));
    let items: Vec<&str> = figcraft_engine::catalog::MENU.iter().filter(|m| m.menu == "Object/AI").map(|m| m.id).collect();
    assert_eq!(items, ["ai.draftCopy", "ai.generateImage"]);
}

#[test]
fn the_first_launch_window_opens_once_and_closing_it_counts_as_later() {
    let mut u = Ui::new(Session::new());
    let info = AppInfo { slug: "figcraft-ui-test", name: "FigCraft", version: "0", app_token: "", dialects: &["chat_completions"], config_dir: None };
    u.app.ai = Some(Ai::new(info));
    for _ in 0..5 {
        u.frame(vec![]);
    }
    assert_eq!(u.app.ui.ai_dialog, Some(AiDialog::Setup), "the first launch asks");
    let shown = texts(&mut u);
    assert!(shows(&shown, "Use my own key"), "{shown:?}");
    escape(&mut u);
    assert_eq!(u.app.ui.ai_dialog, None);
    assert!(ai(&u).state.settings.setup_done, "closing the first-launch window counts as Later");
    for _ in 0..5 {
        u.frame(vec![]);
    }
    assert_eq!(u.app.ui.ai_dialog, None, "it asks once");

    // A run an agent drives doesn't ask.
    let mut u = Ui::new(Session::new());
    let info = AppInfo { slug: "figcraft-ui-test", name: "FigCraft", version: "0", app_token: "", dialects: &["chat_completions"], config_dir: None };
    let mut a = Ai::new(info);
    a.skip_first_run();
    u.app.ai = Some(a);
    for _ in 0..5 {
        u.frame(vec![]);
    }
    assert_eq!(u.app.ui.ai_dialog, None);
}

#[test]
fn without_ai_the_windows_say_so() {
    let mut s = Session::new();
    s.execute("shape.rectangle", &json!({"x": 0, "y": 0, "width": 10, "height": 10})).unwrap();
    let mut u = Ui::new(s);
    crate::menus::invoke(&mut u.app, "ai.generateImage");
    let shown = texts(&mut u);
    assert!(shows(&shown, "AI tools are off"), "{shown:?}");
}

//! Drive the real UI headlessly with synthetic egui input events.

use super::*;
use egui::{Event, Pos2, RawInput, Rect};

struct Rig {
    ctx: egui::Context,
    app: MammothApp,
    t: f64,
}

impl Rig {
    fn new(files: Vec<PathBuf>) -> Self {
        let ctx = egui::Context::default();
        let settings = Settings {
            modules_pinned: false,
            ..Default::default()
        };
        let app = MammothApp::create(&ctx, settings, files);
        let mut rig = Self { ctx, app, t: 0.0 };
        rig.frames(3);
        rig
    }

    fn step(&mut self, events: Vec<Event>) {
        self.t += 1.0 / 60.0;
        let raw = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1400.0, 900.0))),
            time: Some(self.t),
            events,
            ..Default::default()
        };
        let app = &mut self.app;
        let mut out = self.ctx.run_ui(raw, |ui| app.frame(ui));
        out.textures_delta.clear();
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            self.step(vec![]);
        }
    }

    fn key(&mut self, key: Key, modifiers: Modifiers) {
        let ev = |pressed| Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        };
        self.step(vec![ev(true)]);
        self.step(vec![ev(false)]);
        self.frames(2);
    }

    fn text(&mut self, s: &str) {
        self.step(vec![Event::Text(s.to_string())]);
        self.frames(2);
    }

    fn wait_jobs(&mut self) {
        let t = Instant::now();
        let mut last_report = 0u64;
        while !self.app.jobs.is_empty() {
            assert!(t.elapsed() < Duration::from_secs(20), "job timed out");
            std::thread::sleep(Duration::from_millis(5));
            self.step(vec![]);
            let secs = t.elapsed().as_secs();
            if secs > last_report {
                last_report = secs;
                let labels: Vec<&str> = self.app.jobs.iter().map(|j| j.label).collect();
                eprintln!("WAIT {secs}s: {labels:?}");
            }
        }
        self.frames(2);
    }

    fn tab(&self) -> &Tab {
        &self.app.tabs[self.app.active]
    }
}

fn files(tag: &str) -> Vec<PathBuf> {
    let dir = std::env::temp_dir().join(format!("mammoth-ui-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let a = dir.join("people.csv");
    let mut csv = String::from("id,name,email\n");
    for i in 0..500 {
        let name = ["Ava", "Liam", "Zoe", "Noah"][i % 4];
        csv.push_str(&format!(
            "{i},{name},{}{i}@example.com\n",
            name.to_lowercase()
        ));
    }
    std::fs::write(&a, csv).unwrap();
    let b = dir.join("app.log");
    std::fs::write(&b, "[INFO ] started\n[ERROR] failed for bob@example.com\n").unwrap();
    vec![a, b]
}

#[test]
fn keyboard_flow_find_type_undo() {
    let mut rig = Rig::new(files("keys"));
    assert_eq!(rig.app.tabs.len(), 2);
    assert_eq!(rig.app.active, 1, "last opened file is active");

    // Ctrl+Tab switches tabs without touching either document.
    rig.key(Key::Tab, Modifiers::COMMAND);
    assert_eq!(rig.app.active, 0);
    assert!(
        rig.app.tabs.iter().all(|t| !t.doc.is_dirty()),
        "Ctrl+Tab must not edit"
    );

    // Ctrl+F opens the find bar and focuses its field; typing goes there.
    rig.key(Key::F, Modifiers::COMMAND);
    assert!(rig.app.find.open);
    rig.text("Zoe");
    assert_eq!(rig.app.find.query, "Zoe");
    assert!(
        !rig.tab().doc.is_dirty(),
        "typing in the find bar must not edit the document"
    );

    // Enter finds the first match after the cursor (line 3 = "2,Zoe,…").
    rig.key(Key::Enter, Modifiers::NONE);
    rig.wait_jobs();
    let (s, e) = rig.tab().view.selection();
    assert_eq!((s, e), (Pos::new(3, 2), Pos::new(3, 5)));
    // F3 → next match (case-insensitive by default: the "zoe" in the email).
    rig.key(Key::F3, Modifiers::NONE);
    rig.wait_jobs();
    assert_eq!(rig.tab().view.selection().0, Pos::new(3, 6));
    rig.key(Key::F3, Modifiers::NONE);
    rig.wait_jobs();
    assert_eq!(rig.tab().view.selection().0, Pos::new(7, 2));
    rig.key(Key::F3, Modifiers::SHIFT);
    rig.wait_jobs();
    // Shift+F3 → back.
    rig.key(Key::F3, Modifiers::SHIFT);
    rig.wait_jobs();
    assert_eq!(rig.tab().view.selection().0, Pos::new(3, 2));

    // Escape (twice: selection, then find bar) returns to the editor.
    rig.key(Key::Escape, Modifiers::NONE);
    rig.key(Key::Escape, Modifiers::NONE);
    assert!(!rig.app.find.open);
    rig.frames(2);

    // Typing now edits the document at the cursor, and undo restores it.
    rig.text("X");
    rig.text("Y");
    assert!(rig.tab().doc.is_dirty());
    assert_eq!(rig.tab().doc.line(3), "2,ZoeXY,zoe2@example.com");
    rig.key(Key::Z, Modifiers::COMMAND);
    assert_eq!(rig.tab().doc.line(3), "2,Zoe,zoe2@example.com");
    assert!(!rig.tab().doc.is_dirty());
}

#[test]
fn bookmarks_next_error_and_highlight() {
    let mut rig = Rig::new(files("qol"));
    rig.app.active = 1; // app.log: "[INFO ] started\n[ERROR] failed for bob@example.com\n"
    rig.frames(2);

    // Bookmarks: toggle on line 1, jump back to it from line 0, toggle off again.
    rig.app.tabs[1]
        .view
        .select(Pos::new(1, 0), Pos::new(1, 0), Reveal::None);
    rig.app.toggle_bookmark();
    assert_eq!(rig.tab().bookmarks.len(), 1);
    rig.app.tabs[1]
        .view
        .select(Pos::new(0, 0), Pos::new(0, 0), Reveal::None);
    rig.app.jump_bookmark(false);
    assert_eq!(rig.tab().view.selection().0.line, 1);
    rig.app.toggle_bookmark();
    assert!(rig.tab().bookmarks.is_empty());

    // Next error: from the top of the file, jumps to the ERROR token on line 1.
    rig.app.tabs[1]
        .view
        .select(Pos::new(0, 0), Pos::new(0, 0), Reveal::None);
    rig.app
        .jump_to_level(&["ERROR", "FATAL"], "Errors", false, &rig.ctx.clone());
    rig.wait_jobs();
    assert_eq!(rig.tab().view.selection().0, Pos::new(1, 1));

    // Highlight: select "started" on line 0 and toggle a highlight for it.
    rig.app.tabs[1]
        .view
        .select(Pos::new(0, 8), Pos::new(0, 15), Reveal::None);
    rig.app.toggle_highlight_at_cursor();
    assert_eq!(rig.tab().highlights.len(), 1);
    assert_eq!(rig.tab().highlights[0].0, "started");
    rig.app.clear_highlights();
    assert!(rig.tab().highlights.is_empty());
}

#[test]
fn module_count_and_extract() {
    let mut rig = Rig::new(files("modules"));
    rig.app.active = 0;
    rig.frames(2);
    rig.app
        .run_module_action(ModuleAction::Count("email".into()), &rig.ctx.clone());
    rig.wait_jobs();
    assert_eq!(rig.app.find.last_count.map(|c| c.0), Some(500));

    rig.app
        .run_module_action(ModuleAction::Extract("email".into()), &rig.ctx.clone());
    rig.wait_jobs();
    assert_eq!(rig.app.tabs.len(), 3, "extract opens a new tab");
    let t = rig.tab();
    assert_eq!(t.doc.line_count(), 500);
    assert_eq!(t.doc.line(0), "ava0@example.com");
}

#[test]
fn save_over_memory_mapped_file() {
    let dir = std::env::temp_dir().join(format!("mammoth-ui-map-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("huge.txt");
    let mut body = String::new();
    let mut n = 0;
    while body.len() < 70 << 20 {
        body.push_str(&format!(
            "row {n:08} lorem ipsum dolor sit amet
"
        ));
        n += 1;
    }
    std::fs::write(&path, &body).unwrap();

    let mut rig = Rig::new(vec![path.clone()]);
    assert!(rig.tab().doc.source.as_ref().unwrap().is_mapped());
    let sel = (Pos::default(), Pos::default());
    for round in 0..2 {
        // Wait for the (re)index so the whole file is visible.
        let t = Instant::now();
        while !rig.tab().doc.is_fully_loaded() {
            assert!(t.elapsed() < Duration::from_secs(30));
            std::thread::sleep(Duration::from_millis(5));
            rig.step(vec![]);
        }
        assert!(rig.tab().doc.source.as_ref().unwrap().is_mapped());
        let now = rig.app.now;
        let tab = &mut rig.app.tabs[0];
        tab.doc
            .replace(
                Pos::new(0, 0),
                Pos::new(0, 0),
                &format!("EDIT{round} "),
                EditKind::Other,
                sel,
                now,
            )
            .unwrap();
        let last = tab.doc.line_count() - 2;
        tab.doc
            .replace(
                Pos::new(last, 0),
                Pos::new(last, 3),
                "ROW",
                EditKind::Other,
                sel,
                now,
            )
            .unwrap();
        rig.app.save_tab(0, false, &rig.ctx.clone());
        rig.wait_jobs();
        assert!(
            !rig.tab().doc.is_dirty(),
            "round {round}: still dirty after save"
        );
        assert!(rig.tab().doc.busy.is_none());
    }
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(saved.starts_with("EDIT1 EDIT0 row 00000000 lorem"));
    assert_eq!(saved.len(), body.len() + "EDIT1 EDIT0 ".len());
    assert!(saved.ends_with(&format!(
        "ROW {:08} lorem ipsum dolor sit amet
",
        n - 1
    )));
    assert!(
        !dir.join(".huge.txt.mammoth-tmp").exists(),
        "temp file cleaned up"
    );
    // The reopened file is mapped again and shows the saved content.
    let t = Instant::now();
    while rig.tab().doc.line_count() < 1 {
        assert!(t.elapsed() < Duration::from_secs(30));
        rig.step(vec![]);
    }
    assert_eq!(
        rig.tab().doc.line(0),
        "EDIT1 EDIT0 row 00000000 lorem ipsum dolor sit amet"
    );
}

fn preview_rig(tag: &str, lines: usize) -> (Rig, PathBuf) {
    let dir = std::env::temp_dir().join(format!("mammoth-ui-prev-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("big.txt");
    let mut body = String::new();
    for i in 0..lines {
        body.push_str(&format!(
            "line {i:07} some filler text to make the file bigger
"
        ));
    }
    body.push_str(
        "the secret needle is here
",
    );
    std::fs::write(&path, &body).unwrap();
    let ctx = egui::Context::default();
    let settings = Settings {
        preview_threshold_mb: 1,
        preview_lines: 100,
        ..Default::default()
    };
    let app = MammothApp::create(&ctx, settings, vec![path.clone()]);
    let mut rig = Rig { ctx, app, t: 0.0 };
    rig.frames(3);
    (rig, path)
}

fn settle(rig: &mut Rig, done: impl Fn(&Rig) -> bool) {
    let t = Instant::now();
    while !done(rig) {
        assert!(t.elapsed() < Duration::from_secs(20), "timed out");
        std::thread::sleep(Duration::from_millis(5));
        rig.step(vec![]);
    }
    rig.frames(2);
}

#[test]
fn preview_mode_ctrl_l_loads_everything() {
    let (mut rig, _) = preview_rig("ctrl-l", 100_000);
    settle(&mut rig, |r| {
        !r.tab().doc.source.as_ref().unwrap().is_loading()
    });
    assert_eq!(
        rig.tab().doc.line_count(),
        100,
        "preview shows exactly the configured lines"
    );
    assert!(rig.tab().is_partial());
    rig.key(Key::L, Modifiers::COMMAND);
    settle(&mut rig, |r| r.tab().doc.is_fully_loaded());
    assert_eq!(rig.tab().doc.line_count(), 100_002);
}

#[test]
fn preview_mode_find_and_goto_past_loaded_part() {
    let (mut rig, _) = preview_rig("find", 100_000);
    settle(&mut rig, |r| {
        !r.tab().doc.source.as_ref().unwrap().is_loading()
    });

    rig.app.find.query = "secret needle".into();
    rig.app.start_find(false, &rig.ctx.clone());
    settle(&mut rig, |r| {
        r.app.jobs.is_empty() && r.tab().pending_hit.is_none()
    });
    assert_eq!(
        rig.tab().view.selection(),
        (Pos::new(100_000, 4), Pos::new(100_000, 17))
    );
    assert!(
        rig.tab().is_partial(),
        "only loaded up to the match, not the whole file"
    );

    let (mut rig, _) = preview_rig("goto", 100_000);
    settle(&mut rig, |r| {
        !r.tab().doc.source.as_ref().unwrap().is_loading()
    });
    rig.app.goto("54321", &rig.ctx.clone());
    settle(&mut rig, |r| r.tab().pending_goto.is_none());
    assert_eq!(rig.tab().view.cursor, Pos::new(54_320, 0));
    assert_eq!(
        rig.tab().doc.line(54_320),
        "line 0054320 some filler text to make the file bigger"
    );
}

#[test]
fn replace_all_and_save() {
    let paths = files("save");
    let mut rig = Rig::new(paths.clone());
    rig.app.active = 0;
    rig.app.find.query = "Zoe".into();
    rig.app.find.case = true;
    rig.app.find.replace = "Zoë".into();
    rig.app.start_replace_all(0, &rig.ctx.clone());
    rig.wait_jobs();
    assert_eq!(rig.tab().doc.line(3), "2,Zoë,zoe2@example.com");

    rig.app.save_tab(0, false, &rig.ctx.clone());
    rig.wait_jobs();
    assert!(!rig.tab().doc.is_dirty());
    let saved = std::fs::read_to_string(&paths[0]).unwrap();
    assert_eq!(saved.lines().nth(3), Some("2,Zoë,zoe2@example.com"));
    assert_eq!(saved.matches("Zoë").count(), 125);
    assert!(saved.ends_with(".com\n"));
}

#[test]
fn command_palette_runs_module_actions() {
    let mut rig = Rig::new(files("palette"));
    rig.app.active = 0;
    rig.key(Key::P, Modifiers::COMMAND | Modifiers::SHIFT);
    assert!(rig.app.palette.is_some(), "Ctrl+Shift+P opens the palette");
    rig.text("email count");
    let items = rig
        .app
        .palette_items(&rig.app.palette.as_ref().unwrap().query);
    assert_eq!(rig.app.cmd_label(&items[0].0), "Email addresses: Count");
    rig.key(Key::Enter, Modifiers::NONE);
    assert!(
        rig.app.palette.is_none(),
        "Enter runs the command and closes"
    );
    rig.wait_jobs();
    assert_eq!(rig.app.find.last_count.map(|c| c.0), Some(500));
    assert!(
        !rig.tab().doc.is_dirty(),
        "typing in the palette must not edit the document"
    );

    // Arrow keys move the selection; Esc closes without running anything.
    rig.key(Key::P, Modifiers::COMMAND | Modifiers::SHIFT);
    rig.text("syntax");
    rig.key(Key::ArrowDown, Modifiers::NONE);
    assert_eq!(rig.app.palette.as_ref().unwrap().selected, 1);
    rig.key(Key::Escape, Modifiers::NONE);
    assert!(rig.app.palette.is_none());
}

#[test]
fn modules_drawer_toggles_and_pins() {
    let mut rig = Rig::new(files("drawer"));
    assert!(!rig.app.modules_open, "modules are hidden by default");
    rig.key(Key::M, Modifiers::COMMAND | Modifiers::SHIFT);
    assert!(rig.app.modules_open);
    rig.key(Key::Escape, Modifiers::NONE);
    assert!(!rig.app.modules_open, "Esc closes the floating drawer");

    rig.app.run(commands::Cmd::PinModules, &rig.ctx.clone());
    rig.frames(2);
    assert!(rig.app.modules_open && rig.app.settings.modules_pinned);
    rig.key(Key::Escape, Modifiers::NONE);
    assert!(rig.app.modules_open, "a pinned panel stays open");

    // Toggling a detector from the palette's command list works too.
    let was = rig.app.registry.entries[rig.app.registry.by_id("ipv4").unwrap()].enabled;
    rig.app.run(
        commands::Cmd::ToggleDetector("ipv4".into()),
        &rig.ctx.clone(),
    );
    assert_ne!(
        rig.app.registry.entries[rig.app.registry.by_id("ipv4").unwrap()].enabled,
        was
    );
}

/// Renders screenshots of the real UI offscreen (no window, no input stealing):
/// `MAMMOTH_PREVIEW_DIR=dir cargo test render_previews -- --ignored`
#[test]
#[ignore]
fn render_previews() {
    let Ok(dir) = std::env::var("MAMMOTH_PREVIEW_DIR") else {
        return;
    };
    let dir = PathBuf::from(dir);
    let shot = |name: &str,
                app: MammothApp,
                prep: &dyn Fn(&mut MammothApp, &egui::Context)|
     -> MammothApp {
        // MAMMOTH_PREVIEW_ONLY=08 renders just the shots whose name starts with "08".
        if std::env::var("MAMMOTH_PREVIEW_ONLY").is_ok_and(|only| !name.starts_with(only.as_str()))
        {
            let mut app = app;
            prep(&mut app, &egui::Context::default());
            return app;
        }
        let mut harness = egui_kittest::Harness::builder()
            .with_size(vec2(1400.0, 860.0))
            .with_pixels_per_point(1.0)
            .wgpu()
            .build_ui_state(|ui, app: &mut MammothApp| app.frame(ui), app);
        for _ in 0..4 {
            harness.step();
        }
        let ctx = harness.ctx.clone();
        prep(harness.state_mut(), &ctx);
        harness.step();
        for _ in 0..60 {
            harness.step();
            std::thread::sleep(Duration::from_millis(20));
        }
        let img = harness.render().expect("render");
        img.save(dir.join(format!("{name}.png"))).unwrap();
        let _ = &harness;
        let mut app = std::mem::replace(
            harness.state_mut(),
            MammothApp::create(&egui::Context::default(), Settings::default(), vec![]),
        );
        // The next shot runs in a fresh egui context: redo per-context setup.
        app.setup_done = false;
        app.logo = icons::Logo::default();
        app
    };

    let ctx = egui::Context::default();
    let app = MammothApp::create(&ctx, Settings::default(), vec![]);
    shot("01-welcome", app, &|_, _| {});

    let csv = std::env::var("MAMMOTH_PREVIEW_CSV")
        .map(PathBuf::from)
        .unwrap_or_else(|_| files("render")[0].clone());
    let log = files("render")[1].clone();
    let app = MammothApp::create(&ctx, Settings::default(), vec![log, csv]);
    let app = shot("02-editor", app, &|_, _| {});
    let app = shot("03-drawer", app, &|a, _| a.modules_open = true);
    let app = shot("04-palette", app, &|a, _| {
        a.modules_open = false;
        a.open_palette();
        a.palette.as_mut().unwrap().query = "email".into();
    });
    let app = shot("05-pinned", app, &|a, _| {
        a.palette = None;
        a.settings.modules_pinned = true;
        a.modules_open = true;
    });
    let app = shot("06-settings", app, &|a, _| {
        a.modules_open = false;
        a.show_settings = true;
    });
    shot("07-font-picker", app, &|a, _| {
        a.open_font_picker(crate::fonts::Role::Editor);
        std::thread::sleep(Duration::from_millis(1500));
    });

    let app = MammothApp::create(&ctx, Settings::default(), vec![demo_csv()]);
    let app = shot("08-insights", app, &|a, c| {
        a.run_module_action(ModuleAction::Breakdown("email".into()), c);
    });
    let app = shot("09-insights-expanded", app, &|a, _| {
        a.insights_expand_for_test("Gmail")
    });
    let app = shot("09b-insights-addresses", app, &|a, _| {
        a.insights_view_for_test(2)
    });
    let app = shot("10-table", app, &|a, _| {
        a.insights.open = false;
        a.toggle_table();
    });
    shot("11-table-filtered", app, &|a, _| {
        let st = a.tabs[0].table.as_mut().unwrap();
        st.filters.resize(6, String::new());
        st.filters[2] = "yahoo".into();
        st.sort = Some((5, true));
        st.dirty_since = Some(0.0);
    });

    let app = MammothApp::create(&ctx, Settings::default(), vec![json_log("render")]);
    shot("12-json", app, &|a, _| {
        a.toggle_json();
        a.tabs[0].view.cursor = Pos::new(4, 0);
    });

    let app = MammothApp::create(&ctx, Settings::default(), vec![demo_log()]);
    let app = shot("13-heatmap", app, &|a, _| {
        a.tabs[0].view.select(
            Pos::new(6100, 0),
            Pos::new(6100, 0),
            crate::editor::Reveal::Center,
        );
    });
    shot("14-convert", app, &|a, _| {
        a.active = 0;
        a.open_convert(None);
    });
}

fn demo_log() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mammoth-ui-{}-demolog", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("server.log");
    let mut s = String::new();
    for i in 0..20_000usize {
        let burst = (5_900..6_400).contains(&i) || (14_000..14_150).contains(&i);
        let level = if burst && i % 3 != 0 {
            "ERROR"
        } else if i % 97 == 0 {
            "WARN "
        } else {
            "INFO "
        };
        let msg = if i % 23 == 0 || (11_000..11_600).contains(&i) {
            format!(
                "signup user{}@gmail.com from 10.0.{}.{}",
                i,
                i % 255,
                i % 200
            )
        } else if burst {
            format!("db timeout after 30s (pool exhausted) request_id={i:08x}")
        } else {
            format!("GET /api/items/{} 200 {}ms", i % 5000, i % 300)
        };
        s.push_str(&format!(
            "2026-09-25T{:02}:{:02}:{:02}Z [{level}] {msg}\n",
            (i / 3600) % 24,
            (i / 60) % 60,
            i % 60
        ));
    }
    std::fs::write(&p, s).unwrap();
    p
}

fn demo_csv() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mammoth-ui-{}-demo", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("customers.csv");
    let first = [
        "ava", "liam", "zoe", "noah", "mia", "omar", "ivy", "leo", "sara", "yuki", "lena", "raj",
    ];
    let last = [
        "chen", "patel", "kim", "rossi", "muller", "haddad", "nguyen", "santos", "silva", "tanaka",
    ];
    let domains = [
        "gmail.com",
        "gmail.com",
        "gmail.com",
        "gmail.com",
        "yahoo.com",
        "yahoo.co.uk",
        "outlook.com",
        "hotmail.com",
        "icloud.com",
        "proton.me",
        "acme-corp.com",
        "globex.io",
        "stanford.edu",
        "aol.com",
        "gmail.com",
        "hotmail.fr",
        "me.com",
        "initech.net",
    ];
    let countries = ["US", "UK", "DE", "FR", "JP", "BR", "IN", "CA"];
    let mut s = String::from("id,name,email,country,plan,spend\n");
    for i in 0..6000usize {
        let (f, l) = (first[i * 7 % first.len()], last[i * 3 % last.len()]);
        let d = domains[(i * 13 + i / 7) % domains.len()];
        let plan = ["free", "pro", "team"][i * 5 % 3];
        let spend = (i * 37 % 1000) as f64 + (i % 100) as f64 / 100.0;
        s.push_str(&format!(
            "{},{} {},{}.{}{}@{},{},{},{:.2}\n",
            1000 + i,
            capital(f),
            capital(l),
            f,
            l,
            i % 97,
            d,
            countries[i % countries.len()],
            plan,
            spend
        ));
    }
    std::fs::write(&p, s).unwrap();
    p
}

fn capital(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

fn mixed_emails(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mammoth-ui-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("users.csv");
    let domains = [
        "gmail.com",
        "GMAIL.com",
        "yahoo.com",
        "yahoo.co.uk",
        "hotmail.com",
        "acme.io",
        "gmail.com",
        "mit.edu",
    ];
    let mut s = String::from("id,email,plan\n");
    for i in 0..800 {
        s.push_str(&format!("{i},user{i}@{},pro\n", domains[i % domains.len()]));
    }
    std::fs::write(&p, s).unwrap();
    p
}

#[test]
fn email_breakdown_filter_and_jump_back() {
    let mut rig = Rig::new(vec![mixed_emails("breakdown")]);
    // One shortcut opens the insights panel, which analyses the file by itself.
    rig.key(Key::E, Modifiers::COMMAND | Modifiers::SHIFT);
    assert!(rig.app.insights.open);
    let t0 = Instant::now();
    while rig.app.insights_for_test().is_none() {
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "insights never computed"
        );
        std::thread::sleep(Duration::from_millis(5));
        rig.step(vec![]);
    }
    let (total, counts, cats, distinct) = rig.app.insights_for_test().unwrap();
    assert_eq!(total, 800);
    assert_eq!(distinct, 800, "every address is different");
    // Domains are case-folded: gmail.com (3 of every 8) is the top domain.
    assert_eq!(counts[0], ("gmail.com".to_string(), 300));
    assert_eq!(cats[0], ("Gmail".to_string(), 300));
    assert!(cats.contains(&("Yahoo".to_string(), 200)));
    assert!(cats.contains(&("Education".to_string(), 100)));
    assert!(cats.contains(&("Company / other".to_string(), 100)));

    // "Filter" on the Gmail group: a new tab with the header + every Gmail line.
    let sel = GroupSel {
        module: "email".into(),
        label: "Gmail".into(),
        keys: None,
        category: Some("Gmail".into()),
    };
    rig.app.run_row_action(
        breakdown_ui::RowTarget::Group(sel),
        breakdown_ui::RowAction::Filter,
        &rig.ctx.clone(),
    );
    rig.wait_jobs();
    assert_eq!(rig.app.tabs.len(), 2);
    let t = rig.tab();
    assert_eq!(t.doc.line_count(), 301);
    assert_eq!(t.doc.line(0), "id,email,plan");
    assert_eq!(t.doc.line(1), "0,user0@gmail.com,pro");
    assert_eq!(t.doc.line(2), "1,user1@GMAIL.com,pro");
    assert_eq!(t.origin_lines().unwrap()[2], 2);

    // Double-clicking a line number jumps back to that line in the source.
    rig.app.jump_to_source(1, 2);
    assert_eq!(rig.app.active, 0);
    assert_eq!(rig.tab().view.cursor.line, 2);
}

#[test]
fn find_bar_filter_button_uses_the_query() {
    let mut rig = Rig::new(vec![mixed_emails("filter-text")]);
    rig.app.find.query = "yahoo".into();
    rig.app.start_filter(&rig.ctx.clone());
    rig.wait_jobs();
    let t = rig.tab();
    assert_eq!(t.doc.line_count(), 201, "header + 200 yahoo lines");
    assert!(t.doc.line(1).contains("yahoo.com"));
}

#[test]
fn csv_table_filter_sort_edit_export() {
    let mut rig = Rig::new(vec![mixed_emails("table")]);
    assert_eq!(rig.tab().syntax.as_deref(), Some("csv"));
    rig.key(Key::T, Modifiers::COMMAND | Modifiers::SHIFT);
    assert!(rig.tab().table_mode, "Ctrl+Shift+T switches to table view");
    assert_eq!(
        rig.tab().table.as_ref().unwrap().row_count(&rig.tab().doc),
        800
    );

    // Filter the email column to Gmail, sort by id descending.
    {
        let st = rig.app.tabs[0].table.as_mut().unwrap();
        st.filters.resize(3, String::new());
        st.filters[1] = "gmail".into();
        st.sort = Some((0, true));
        st.dirty_since = Some(0.0);
    }
    let t0 = Instant::now();
    while rig.tab().table.as_ref().unwrap().view.is_none() {
        assert!(t0.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(5));
        rig.step(vec![]);
    }
    let st = rig.tab().table.as_ref().unwrap();
    let view = st.view.clone().unwrap();
    assert_eq!(view.len(), 300);
    // Highest id first: row 798 is "798,user798@gmail.com" (798 % 8 == 6 → gmail.com).
    assert_eq!(rig.tab().doc.line(view[0]), "798,user798@gmail.com,pro");

    // Edit the plan cell of the first row: select it, type, press Enter.
    rig.app.tabs[0].table.as_mut().unwrap().sel = (0, 2);
    rig.frames(2);
    rig.text("enterprise");
    rig.key(Key::Enter, Modifiers::NONE);
    assert_eq!(
        rig.tab().doc.line(view[0]),
        "798,user798@gmail.com,enterprise"
    );
    assert!(rig.tab().doc.is_dirty());
    rig.key(Key::Z, Modifiers::COMMAND);
    assert_eq!(
        rig.tab().doc.line(view[0]),
        "798,user798@gmail.com,pro",
        "cell edits are undoable"
    );

    // "Open view as tab": header + the 300 rows in view order.
    rig.app.start_table_export(0, &rig.ctx.clone());
    rig.wait_jobs();
    assert_eq!(rig.app.tabs.len(), 2);
    let t = &rig.app.tabs[1];
    assert_eq!(t.doc.line_count(), 301);
    assert_eq!(t.doc.line(0), "id,email,plan");
    assert_eq!(t.doc.line(1), "798,user798@gmail.com,pro");
}

fn json_log(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mammoth-ui-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("app.log");
    let mut s = String::new();
    for i in 0..300 {
        let status = if i % 10 == 0 { 503 } else { 200 };
        s.push_str(&format!(
            "2026-09-25T10:00:{:02}Z [INFO ] request {{\"id\":{i},\"status\":{status},\"user\":{{\"email\":\"u{i}@gmail.com\"}}}}\n",
            i % 60
        ));
        if i % 50 == 0 {
            s.push_str("plain text line without json\n");
        }
    }
    std::fs::write(&p, s).unwrap();
    p
}

#[test]
fn json_inspector_and_jq_on_file() {
    let mut rig = Rig::new(vec![json_log("json")]);
    rig.key(Key::J, Modifiers::COMMAND | Modifiers::SHIFT);
    assert!(rig.app.json.open);
    rig.app.tabs[0].view.cursor = Pos::new(2, 0);
    rig.frames(2);
    // Line 3 is request 1 (line 2 is plain text): the JSON after the log prefix is found.
    let first = rig.app.json_values().expect("values");
    assert_eq!(first[0]["user"]["email"], "u1@gmail.com");

    // Live jq on the current line.
    rig.app.json.jq = ".user.email".into();
    rig.frames(2);
    assert_eq!(
        rig.app.json_values().unwrap(),
        vec![serde_json::json!("u1@gmail.com")]
    );

    // Run over the whole file: one output per JSON line; plain lines are skipped.
    rig.app.start_jq_run(0, &rig.ctx.clone());
    rig.wait_jobs();
    assert_eq!(rig.app.tabs.len(), 2);
    assert_eq!(rig.app.tabs[1].doc.line_count(), 300);
    assert_eq!(rig.app.tabs[1].doc.line(0), "\"u0@gmail.com\"");

    // "Keep matching lines" with select(): the original log lines of 5xx requests.
    rig.app.active = 0;
    rig.app.json.jq = "select(.status >= 500)".into();
    rig.app.json.run_keep_lines_for_test(true);
    rig.app.start_jq_run(0, &rig.ctx.clone());
    rig.wait_jobs();
    let t = &rig.app.tabs[2];
    assert_eq!(t.doc.line_count(), 30);
    assert!(
        t.doc
            .line(0)
            .starts_with("2026-09-25T10:00:00Z [INFO ] request {\"id\":0,\"status\":503")
    );
}

#[test]
fn pretty_print_and_minify_document() {
    let dir = std::env::temp_dir().join(format!("mammoth-ui-{}-pretty", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("data.json");
    std::fs::write(
        &p,
        r#"{"b":1,"a":[1,2,{"c":null}],"n":12345678901234567890}"#,
    )
    .unwrap();
    let mut rig = Rig::new(vec![p]);
    rig.app.reformat_json(true);
    let d = &rig.tab().doc;
    assert!(d.line_count() > 5);
    assert_eq!(d.line(1), r#"  "b": 1,"#, "key order is kept");
    rig.app.reformat_json(false);
    assert_eq!(
        rig.tab().doc.line(0),
        r#"{"b":1,"a":[1,2,{"c":null}],"n":12345678901234567890}"#
    );
    rig.key(Key::Z, Modifiers::COMMAND);
    assert!(
        rig.tab().doc.line_count() > 5,
        "undo restores the pretty version"
    );
}

#[test]
fn heatmap_scans_and_follows_detector_changes() {
    let mut rig = Rig::new(vec![mixed_emails("heat")]);
    let t0 = Instant::now();
    while rig.tab().heat.map_for_test().is_none() {
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "heatmap never computed"
        );
        std::thread::sleep(Duration::from_millis(5));
        rig.step(vec![]);
    }
    // Email + URL detectors are on by default: two layers, 800 emails.
    let (layers, totals) = rig.tab().heat.map_for_test().unwrap();
    assert_eq!(
        layers,
        vec!["Email addresses".to_string(), "URLs".to_string()]
    );
    assert_eq!(totals, vec![800, 0]);

    // Turning on another detector rescans (after a short pause).
    rig.app.run(
        commands::Cmd::ToggleDetector("uuid".into()),
        &rig.ctx.clone(),
    );
    let t0 = Instant::now();
    while rig.tab().heat.map_for_test().unwrap().0.len() != 3 {
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "heatmap didn't rescan"
        );
        std::thread::sleep(Duration::from_millis(20));
        rig.step(vec![]);
    }
}

#[test]
fn convert_csv_to_json_lines_tab_and_file() {
    use crate::convert::{Format, Spec};
    let src = mixed_emails("convert");
    let mut rig = Rig::new(vec![src.clone()]);
    rig.app.open_convert(None);
    assert!(rig.app.convert.is_some(), "dialog opens");
    rig.app.convert = None;

    let spec = Spec {
        from: Format::Delimited(b','),
        to: Format::JsonLines,
        header: true,
        typed: true,
        flatten: true,
    };
    rig.app
        .start_convert(0, spec.clone(), None, &rig.ctx.clone());
    rig.wait_jobs();
    let t = rig.tab();
    assert_eq!(t.doc.title, "users.jsonl");
    assert_eq!(t.doc.line_count(), 800);
    assert_eq!(
        t.doc.line(0),
        r#"{"id":0,"email":"user0@gmail.com","plan":"pro"}"#
    );

    // To a file: written, then opened.
    let out = src.with_file_name("users-out.jsonl");
    rig.app.active = 0;
    rig.app
        .start_convert(0, spec, Some(out.clone()), &rig.ctx.clone());
    rig.wait_jobs();
    let written = std::fs::read_to_string(&out).unwrap();
    assert_eq!(written.lines().count(), 800);
    assert_eq!(rig.tab().doc.path.as_deref(), Some(out.as_path()));

    // And back: JSON Lines → CSV reproduces the original rows.
    let back = Spec {
        from: Format::JsonLines,
        to: Format::Delimited(b','),
        header: true,
        typed: true,
        flatten: true,
    };
    let active = rig.app.active;
    rig.app.start_convert(active, back, None, &rig.ctx.clone());
    rig.wait_jobs();
    assert_eq!(rig.tab().doc.line(0), "id,email,plan");
    assert_eq!(rig.tab().doc.line(2), "1,user1@GMAIL.com,pro");
}

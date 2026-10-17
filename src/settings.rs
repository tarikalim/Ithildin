//! The Settings dialog: appearance, the sources and where they read from, and the database.
//! Writes go through its own database connection; the window picks them up by data_version.

use std::path::Path;

use anyhow::{Context as _, anyhow};
use chrono::{Local, TimeZone as _};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable as _, Selectable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;

use crate::contract::Description;
use crate::store::{SourceSetting, Store};
use crate::theme::pal;

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Appearance,
    Sources,
    Storage,
}

pub struct Settings {
    store: Store,
    tab: Tab,
    sources: Vec<SourceSetting>,
    /// The source being added or edited, in place of the list.
    editor: Option<Editor>,
    error: Option<String>,
}

struct Editor {
    /// The source's name when editing; None when adding.
    editing: Option<String>,
    name: Entity<InputState>,
    title: Entity<InputState>,
    config: Entity<TextareaState>,
    /// The last preview, or why it failed.
    preview: Option<Result<String, String>>,
    busy: bool,
}

/// A starting config per source type; the ithildin skill documents every key.
fn template(kind: &str) -> Value {
    match kind {
        "git-paths" => {
            serde_json::json!({ "type": "git-paths", "repo": "", "patterns": ["{state}/{label}/**"], "entity": "{label}" })
        }
        _ => {
            serde_json::json!({ "type": "jsonl", "files": "", "rules": [{ "where": {}, "at": "timestamp", "entity": "{id}", "kind": "event" }] })
        }
    }
}

/// `~/x` for a path under the home folder.
fn tilde(path: &Path) -> String {
    let home = dirs::home_dir().unwrap_or_default();
    match path.strip_prefix(&home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// The dry run's summary as a few lines of text.
fn summary(v: &Value) -> String {
    let kinds = v["kinds"]
        .as_object()
        .map(|k| {
            k.iter()
                .map(|(k, n)| format!("{k} {n}"))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let mut out = format!(
        "{} events · {} entities · {kinds}\n{} → {}",
        v["events"],
        v["entities"],
        v["first"].as_str().unwrap_or("–"),
        v["last"].as_str().unwrap_or("–")
    );
    for e in v["samples"].as_array().into_iter().flatten() {
        let text = e["data"]["subject"]
            .as_str()
            .or(e["data"]["to"].as_str())
            .unwrap_or_default();
        let name = e["label"]
            .as_str()
            .or(e["entity"].as_str())
            .unwrap_or_default();
        out.push_str(&format!(
            "\n{}  {}  {}  {}",
            e["at"].as_str().unwrap_or_default(),
            name,
            e["kind"].as_str().unwrap_or_default(),
            text
        ));
    }
    out
}

impl Settings {
    pub fn new(_: &mut Window, _: &mut Context<Self>) -> Self {
        let mut this = Settings {
            store: Store::open_default().expect("open database"),
            tab: Tab::Appearance,
            sources: vec![],
            editor: None,
            error: None,
        };
        this.reload();
        this
    }

    fn reload(&mut self) {
        match self.store.source_settings() {
            Ok(rows) => self.sources = rows,
            Err(e) => self.error = Some(format!("{e:#}")),
        }
    }

    fn result(&mut self, r: anyhow::Result<()>, cx: &mut Context<Self>) {
        self.error = r.err().map(|e| format!("{e:#}"));
        self.reload();
        cx.notify();
    }

    /// Opens the editor on a source's config, or on a template for a new one.
    fn open_editor(
        &mut self,
        source: Option<&SourceSetting>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let config = source
            .and_then(|s| s.config.clone())
            .unwrap_or_else(|| template("jsonl"));
        let text = serde_json::to_string_pretty(&config).unwrap_or_default();
        let (name, title) = source
            .map(|s| (s.name.clone(), s.title.clone().unwrap_or_default()))
            .unwrap_or_default();
        self.editor = Some(Editor {
            editing: source.map(|s| s.name.clone()),
            name: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(name)
                    .placeholder("name, e.g. notes")
            }),
            title: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(title)
                    .placeholder("title above the board")
            }),
            config: cx.new(|cx| TextareaState::new(window, cx).rows(16).default_value(text)),
            preview: None,
            busy: false,
        });
        self.error = None;
        cx.notify();
    }

    fn set_config(&mut self, config: &Value, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ed) = &self.editor else { return };
        let text = serde_json::to_string_pretty(config).unwrap_or_default();
        ed.config.update(cx, |c, cx| c.set_value(text, window, cx));
    }

    /// The form as a description and a config, or what is wrong with it.
    fn form(&self, cx: &App) -> anyhow::Result<(Description, Value)> {
        let ed = self.editor.as_ref().context("no editor")?;
        let name = ed.name.read(cx).value().trim().to_string();
        if name.is_empty() {
            return Err(anyhow!("give the source a name"));
        }
        let title = ed.title.read(cx).value().trim().to_string();
        let config: Value = serde_json::from_str(&ed.config.read(cx).value())
            .context("the config is not valid JSON")?;
        crate::sources::parse(&config).context("config")?;
        let columns = self
            .store
            .description(&name)?
            .map(|d| d.columns)
            .unwrap_or_default();
        Ok((
            Description {
                source: name,
                title: (!title.is_empty()).then_some(title),
                columns,
            },
            config,
        ))
    }

    /// Asks macOS for a folder and puts it in the config: a git repo, or the folder whose
    /// JSON Lines files (any depth) are read.
    fn choose_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else {
                return;
            };
            let Some(dir) = paths.first().cloned() else {
                return;
            };
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(ed) = &this.editor else { return };
                let mut config: Value = serde_json::from_str(&ed.config.read(cx).value())
                    .unwrap_or_else(|_| template("jsonl"));
                match config["type"].as_str() {
                    Some("git-paths") => config["repo"] = Value::String(tilde(&dir)),
                    _ => config["files"] = Value::String(format!("{}/**.jsonl", tilde(&dir))),
                }
                this.set_config(&config, window, cx);
            });
        })
        .detach();
    }

    /// Reads the form's config into a scratch database in the background and shows the result.
    fn preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let form = self.form(cx);
        let Some(ed) = &mut self.editor else { return };
        let (d, config) = match form {
            Ok(f) => f,
            Err(e) => {
                ed.preview = Some(Err(format!("{e:#}")));
                return cx.notify();
            }
        };
        ed.busy = true;
        cx.notify();
        let job = cx.background_spawn(async move {
            crate::mcp::dry_run(&d, &config)
                .map(|v| summary(&v))
                .map_err(|e| format!("{e:#}"))
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = job.await;
            let _ = this.update(cx, |this, cx| {
                if let Some(ed) = &mut this.editor {
                    (ed.preview, ed.busy) = (Some(result), false);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Saves the source; the reader picks it up within seconds (a changed config rebuilds it).
    fn save(&mut self, cx: &mut Context<Self>) {
        let r = self
            .form(cx)
            .and_then(|(d, config)| self.store.configure(&d, &config).map(|_| ()));
        if r.is_ok() {
            self.editor = None;
        }
        self.result(r, cx);
    }

    fn editor(&self, ed: &Editor, cx: &Context<Self>) -> Div {
        let adding = ed.editing.is_none();
        let kind = serde_json::from_str::<Value>(&ed.config.read(cx).value())
            .ok()
            .and_then(|c| c["type"].as_str().map(str::to_string));
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(div().flex_1().child(heading(if adding {
                        "New source"
                    } else {
                        "Edit source"
                    })))
                    .child(
                        Button::new("cancel")
                            .label("Cancel")
                            .ghost()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.editor = None;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(
                        div()
                            .w(px(180.))
                            .child(Input::new(&ed.name).disabled(!adding)),
                    )
                    .child(div().flex_1().child(Input::new(&ed.title))),
            )
            .child(
                div()
                    .flex()
                    .gap(px(6.))
                    .children(["git-paths", "jsonl"].map(|t| {
                        Button::new(t)
                            .label(t)
                            .outline()
                            .selected(kind.as_deref() == Some(t))
                            .disabled(!adding)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.set_config(&template(t), window, cx)
                            }))
                    }))
                    .child(div().flex_1())
                    .child(
                        Button::new("folder")
                            .label("Choose folder…")
                            .outline()
                            .on_click(
                                cx.listener(|this, _, window, cx| this.choose_folder(window, cx)),
                            ),
                    ),
            )
            .child(Textarea::new(&ed.config).h(px(260.)))
            .child(div().text_size(px(11.)).text_color(rgb(pal().muted)).child(
                "Every key and condition is described in the ithildin skill (skills/ithildin).",
            ))
            .child(
                div()
                    .flex()
                    .gap(px(6.))
                    .justify_end()
                    .child(
                        Button::new("preview")
                            .label(if ed.busy { "Reading…" } else { "Preview" })
                            .outline()
                            .disabled(ed.busy)
                            .on_click(cx.listener(|this, _, window, cx| this.preview(window, cx))),
                    )
                    .child(
                        Button::new("save")
                            .label("Save")
                            .primary()
                            .disabled(ed.busy)
                            .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                    ),
            )
            .when_some(ed.preview.clone(), |el, p| {
                let (text, color) = match p {
                    Ok(t) => (t, pal().text),
                    Err(e) => (e, pal().error),
                };
                el.child(
                    div()
                        .p(px(10.))
                        .rounded(px(6.))
                        .bg(rgb(pal().bg))
                        .text_size(px(12.))
                        .text_color(rgb(color))
                        .children(
                            text.lines()
                                .map(|l| div().child(l.to_string()))
                                .collect::<Vec<_>>(),
                        ),
                )
            })
    }

    fn appearance(&self) -> Div {
        let current = crate::theme::saved_choice();
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(heading("Theme"))
            .children(crate::theme::available().into_iter().map(|(name, p)| {
                let chosen = name == current;
                let pick = name.clone();
                div()
                    .id(SharedString::from(format!("theme-{name}")))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .px(px(10.))
                    .py(px(7.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .when(chosen, |el| el.bg(rgb(pal().selected)))
                    .hover(|s| s.bg(rgb(pal().hover)))
                    // Swatch: the theme's background, surface and accent.
                    .child(
                        div()
                            .flex()
                            .rounded(px(4.))
                            .overflow_hidden()
                            .border_1()
                            .border_color(rgb(pal().border))
                            .children(
                                [p.bg, p.surface, p.accent]
                                    .map(|c| div().w(px(12.)).h(px(18.)).bg(rgb(c))),
                            ),
                    )
                    .child(div().flex_1().child(name))
                    .child(
                        div()
                            .text_color(rgb(pal().muted))
                            .text_size(px(12.))
                            .child(if p.dark { "dark" } else { "light" }),
                    )
                    .on_click(move |_, _, cx| {
                        crate::theme::save_choice(&pick);
                        crate::ui::apply_theme(&pick, cx);
                    })
            }))
    }

    fn sources(&self, cx: &Context<Self>) -> Div {
        if let Some(ed) = &self.editor {
            return self.editor(ed, cx);
        }
        let add = div().flex().justify_end().child(
            Button::new("add-source")
                .label("+ Add source")
                .outline()
                .on_click(cx.listener(|this, _, window, cx| this.open_editor(None, window, cx))),
        );
        div().flex().flex_col().gap(px(14.)).child(add).children(
            self.sources.iter().enumerate().map(|(ix, s)| {
                let name = s.name.clone();
                let kind = s
                    .config
                    .as_ref()
                    .and_then(|c| c["type"].as_str())
                    .unwrap_or("fed from outside (MCP, ingest)")
                    .to_string();
                let synced = s
                    .synced_at
                    .map(|t| format!(" · read {}", time(t)))
                    .unwrap_or_default();
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .p(px(12.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(rgb(pal().border))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(
                                div()
                                    .flex_1()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div().child(
                                            s.title.clone().unwrap_or_else(|| s.name.clone()),
                                        ),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(12.))
                                            .text_color(rgb(pal().muted))
                                            .child(format!(
                                                "{} · {kind} · {} events{synced}",
                                                s.name, s.events
                                            )),
                                    ),
                            )
                            .when(s.config.is_some(), |el| {
                                el.child(
                                    Button::new(SharedString::from(format!("edit-{name}")))
                                        .label("Edit")
                                        .ghost()
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            let source = this.sources.get(ix).cloned();
                                            this.open_editor(source.as_ref(), window, cx);
                                        })),
                                )
                            })
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(rgb(pal().muted))
                                    .child("Shown"),
                            )
                            .child(
                                Switch::new(SharedString::from(format!("shown-{name}")))
                                    .checked(!s.hidden)
                                    .on_click(cx.listener({
                                        let name = name.clone();
                                        move |this, on: &bool, _, cx| {
                                            let r = this.store.set_hidden(&name, !*on);
                                            this.result(r, cx);
                                        }
                                    })),
                            ),
                    )
                    .when_some(s.error.clone(), |el, e| {
                        el.child(
                            div()
                                .text_size(px(12.))
                                .text_color(rgb(pal().error))
                                .child(e),
                        )
                    })
            }),
        )
    }

    fn storage(&self) -> Div {
        let db = Store::default_path();
        let size: u64 = ["", "-wal", "-shm"]
            .iter()
            .filter_map(|suffix| std::fs::metadata(format!("{}{suffix}", db.display())).ok())
            .map(|m| m.len())
            .sum();
        let total: i64 = self.sources.iter().map(|s| s.events).sum();
        let dir = db.clone();
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(heading("Database"))
            .child(row("Size", format!("{:.1} MB", size as f64 / 1_048_576.0)))
            .child(row("Events", total.to_string()))
            .children(
                self.sources
                    .iter()
                    .map(|s| row(&format!("  {}", s.name), s.events.to_string())),
            )
            .child(row("File", db.display().to_string()))
            .child(
                div().child(
                    Button::new("reveal")
                        .label("Show in Finder")
                        .outline()
                        .on_click(move |_, _, _| {
                            if let Err(e) = std::process::Command::new("open")
                                .arg("-R")
                                .arg(&dir)
                                .spawn()
                            {
                                eprintln!("show in finder: {e}");
                            }
                        }),
                ),
            )
    }
}

impl Render for Settings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs = [
            (Tab::Appearance, "Appearance"),
            (Tab::Sources, "Sources"),
            (Tab::Storage, "Storage"),
        ];
        div()
            .flex()
            .gap(px(16.))
            .h(px(460.))
            .text_size(px(13.))
            .child(
                div()
                    .w(px(130.))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .children(tabs.map(|(tab, label)| {
                        div()
                            .id(label)
                            .px(px(10.))
                            .py(px(7.))
                            .rounded(px(6.))
                            .cursor_pointer()
                            .when(self.tab == tab, |el| el.bg(rgb(pal().selected)))
                            .hover(|s| s.bg(rgb(pal().hover)))
                            .child(label)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.tab = tab;
                                cx.notify();
                            }))
                    })),
            )
            .child(
                div()
                    .id("settings-page")
                    .flex_1()
                    .min_w_0()
                    .overflow_y_scroll()
                    .when_some(self.error.clone(), |el, e| {
                        el.child(div().pb(px(8.)).text_color(rgb(pal().error)).child(e))
                    })
                    .child(match self.tab {
                        Tab::Appearance => self.appearance(),
                        Tab::Sources => self.sources(cx),
                        Tab::Storage => self.storage(),
                    }),
            )
    }
}

fn heading(title: &str) -> Div {
    div()
        .pb(px(4.))
        .text_size(px(11.))
        .text_color(rgb(pal().muted))
        .child(title.to_uppercase())
}

fn row(label: &str, value: String) -> Div {
    div()
        .flex()
        .gap(px(12.))
        .child(
            div()
                .w(px(110.))
                .flex_none()
                .text_color(rgb(pal().muted))
                .child(label.to_string()),
        )
        .child(div().flex_1().min_w_0().child(value))
}

fn time(ms: i64) -> String {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format("%a %d %b %H:%M").to_string())
        .unwrap_or_default()
}

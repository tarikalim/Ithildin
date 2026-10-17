//! The window: a board for the moment under the playhead, the timeline to move it, and a panel
//! with the metadata and history of the clicked entity. Knows only contract v1, no source.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use chrono::{Datelike as _, Days, Local, NaiveDate, TimeZone as _};
use gpui_kit::component::Disableable as _;
use gpui_kit::component::IndexPath;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::calendar::Date;
use gpui_kit::component::date_picker::{
    DatePicker, DatePickerEvent, DatePickerState, DateRangePreset, DateTime,
};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{IconName, WindowExt as _};
use gpui_kit::component::{Theme, ThemeConfig, ThemeMode};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;

use crate::history::{Card, History, Lane, field};
use crate::sources::Opened;
use crate::store::{Event, Store};
use crate::theme::*;

/// Playback covers the whole history in about this long, idle hours skipped.
const PLAY_SECONDS: f64 = 40.0;
/// Playback speeds, YouTube's.
const SPEEDS: &[(&str, f64)] = &[
    ("0.25x", 0.25),
    ("0.5x", 0.5),
    ("0.75x", 0.75),
    ("1x", 1.0),
    ("1.25x", 1.25),
    ("1.5x", 1.5),
    ("1.75x", 1.75),
    ("2x", 2.0),
];
/// During playback, quiet stretches longer than this are jumped over.
const IDLE_SKIP_MS: i64 = 45 * 60 * 1000;
const FRAME: Duration = Duration::from_millis(16);
const BUCKETS: usize = 240;
const PANEL_WIDTH: f32 = 380.;
const HISTORY_ROWS: usize = 40;
const HOUR: i64 = 3600 * 1000;
/// Stream view zoom steps: how much time the lanes show.
const MINUTE: i64 = 60 * 1000;
const DEFAULT_SPAN: i64 = 6 * HOUR;
/// Pinch zoom limits for the stream view.
const MIN_SPAN: i64 = MINUTE;
const MAX_SPAN: i64 = 30 * 24 * HOUR;
/// Ready spans in the zoom menu.
const SPANS: &[i64] = &[
    5 * MINUTE,
    15 * MINUTE,
    30 * MINUTE,
    HOUR,
    3 * HOUR,
    6 * HOUR,
    12 * HOUR,
    24 * HOUR,
    72 * HOUR,
    168 * HOUR,
];
/// Where the playhead sits across the lanes: mostly the past, a little of what comes next.
const PLAYHEAD_AT: f32 = 0.75;
const LANE_LABEL: f32 = 150.;
const LANE_HEIGHT: f32 = 26.;
/// Card sizes from roomy to dense (width, height, text); a column uses the largest that fits
/// and scrolls only when even the smallest does not.
const CARD_SIZES: &[(f32, f32, f32)] = &[(96., 26., 12.), (76., 22., 11.), (60., 18., 10.)];
const CARD_GAP: f32 = 6.;
/// Data fields the window already shows in its own way; the rest is listed as metadata.
const SHOWN_FIELDS: &[&str] = &["to", "from", "label", "group", "links"];

pub fn run() {
    crate::sources::watch();
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            apply_theme(&crate::theme::saved_choice(), cx);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1440.), px(900.)), cx)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Ithildin".into()),
                    ..Default::default()
                }),
                app_id: Some("ithildin".into()),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| Timeline::new(window, cx))
            })
            .expect("open window");
            cx.activate(true);
        });
}

/// Draws the window and gpui-component's own widgets (date picker, menus, tooltips) with the
/// named theme; an unknown name falls back to the default.
pub(crate) fn apply_theme(name: &str, cx: &mut App) {
    let themes = crate::theme::available();
    let palette = themes
        .iter()
        .find(|(n, _)| n == name)
        .unwrap_or(&themes[0])
        .1;
    crate::theme::set(palette);
    match serde_json::from_value::<ThemeConfig>(crate::theme::component_theme(&palette)) {
        Ok(config) if palette.dark => Theme::global_mut(cx).dark_theme = Rc::new(config),
        Ok(config) => Theme::global_mut(cx).light_theme = Rc::new(config),
        Err(e) => eprintln!("component theme: {e}"),
    }
    Theme::change(
        if palette.dark {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        },
        None,
        cx,
    );
    cx.refresh_windows();
}

#[derive(Clone, Copy, PartialEq)]
enum View {
    /// `state` events as columns.
    Board,
    /// Moments as dots on one lane per entity.
    Stream,
}

pub struct Timeline {
    store: Store,
    /// Every source with events; the window shows one of them.
    sources: Vec<String>,
    view: View,
    /// How much time the stream view shows, in ms; a trackpad pinch changes it.
    span: i64,
    /// The dates shown, start inclusive and end exclusive, in unix ms; None shows everything.
    range: Option<(i64, i64)>,
    range_picker: Entity<DatePickerState>,
    _range_changes: Subscription,
    source_picker: Entity<SelectState<Vec<String>>>,
    speed_picker: Entity<SelectState<Vec<&'static str>>>,
    view_picker: Entity<SelectState<Vec<String>>>,
    _picks: [Subscription; 3],
    /// Playback speed, a factor of SPEEDS.
    speed: f64,
    source: Option<String>,
    history: History,
    /// Entity whose panel is open.
    selected: Option<String>,
    /// Stream lanes whose child lanes are shown.
    expanded: std::collections::HashSet<String>,
    /// Playhead, unix ms.
    t: i64,
    seen_version: i64,
    error: Option<String>,
    playing: Option<Task<()>>,
    scrubbing: bool,
    /// Where the timeline bar was painted last frame, to map the mouse to a time.
    bar: Rc<Cell<Bounds<Pixels>>>,
    /// The board's or stream's area last frame: sizes cards so a column fits, turns swipes into time.
    area: Rc<Cell<Bounds<Pixels>>>,
    _watch: Task<()>,
}

impl Timeline {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let store = Store::open_default().expect("open database");
        // ponytail: polls data_version once a second (~2 µs); a file watcher if it ever matters.
        let watch = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if this
                    .update_in(cx, |this, window, cx| this.reload_if_changed(window, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        let (this_week_start, this_week_end) = week(today(), 0);
        let range_picker = cx.new(|cx| {
            let mut state =
                DatePickerState::range(window, cx).first_day_of_week(chrono::Weekday::Mon);
            state.set_date(
                Date::Range(Some(this_week_start), Some(this_week_end)),
                window,
                cx,
            );
            state
        });
        let range_changes = cx.subscribe_in(
            &range_picker,
            window,
            |this, _, ev: &DatePickerEvent, _, cx| {
                let DatePickerEvent::Change(value) = ev;
                this.range = match value {
                    DateTime::Range(Some(from), Some(to)) => {
                        Some((day_start(from.date()), day_start(to.date() + Days::new(1))))
                    }
                    DateTime::Range(None, None) => None,
                    _ => return, // half a range: wait for the second date
                };
                this.playing = None;
                this.apply_range();
                this.t = this.history.end;
                cx.notify();
            },
        );
        let source_picker = cx.new(|cx| SelectState::new(Vec::<String>::new(), None, window, cx));
        let speed_picker = cx.new(|cx| {
            let normal = SPEEDS.iter().position(|(_, f)| *f == 1.0);
            SelectState::new(
                SPEEDS.iter().map(|(l, _)| *l).collect::<Vec<_>>(),
                normal.map(IndexPath::new),
                window,
                cx,
            )
        });
        let view_picker = cx.new(|cx| SelectState::new(Vec::<String>::new(), None, window, cx));
        let picks = [
            cx.subscribe_in(
                &source_picker,
                window,
                |this, _, ev: &SelectEvent<Vec<String>>, window, cx| {
                    let SelectEvent::Confirm(Some(source)) = ev else {
                        return;
                    };
                    this.show_source(source.clone(), window, cx);
                },
            ),
            cx.subscribe_in(
                &view_picker,
                window,
                |this, _, ev: &SelectEvent<Vec<String>>, _, cx| {
                    let SelectEvent::Confirm(Some(label)) = ev else {
                        return;
                    };
                    this.view = if label == "Board" {
                        View::Board
                    } else {
                        View::Stream
                    };
                    cx.notify();
                },
            ),
            cx.subscribe_in(
                &speed_picker,
                window,
                |this, _, ev: &SelectEvent<Vec<&'static str>>, _, cx| {
                    let SelectEvent::Confirm(Some(label)) = ev else {
                        return;
                    };
                    this.speed = SPEEDS
                        .iter()
                        .find(|(l, _)| l == label)
                        .map_or(1.0, |(_, f)| *f);
                    cx.notify();
                },
            ),
        ];
        let mut this = Timeline {
            seen_version: store.data_version().unwrap_or_default(),
            store,
            sources: vec![],
            view: View::Board,
            span: DEFAULT_SPAN,
            range: Some((
                day_start(this_week_start),
                day_start(this_week_end + Days::new(1)),
            )),
            range_picker,
            _range_changes: range_changes,
            source_picker,
            speed_picker,
            view_picker,
            _picks: picks,
            speed: 1.0,
            source: None,
            history: History::empty(),
            selected: None,
            expanded: Default::default(),
            t: 0,
            error: None,
            playing: None,
            scrubbing: false,
            bar: Rc::default(),
            area: Rc::default(),
            _watch: watch,
        };
        this.load();
        this.sync_pickers(window, cx);
        this.t = this.history.end;
        this
    }

    /// Keeps the source and view dropdowns in step with the loaded source: a source offers
    /// only the views it has events for.
    fn sync_pickers(&self, window: &mut Window, cx: &mut Context<Self>) {
        let (sources, source) = (self.sources.clone(), self.source.clone());
        self.source_picker.update(cx, |picker, cx| {
            picker.set_items(sources, window, cx);
            match &source {
                Some(s) => picker.set_selected_value(s, window, cx),
                None => picker.set_selected_index(None, window, cx),
            }
        });
        let views: Vec<String> = [
            (self.history.has_states(), "Board"),
            (self.history.has_moments(), "Stream"),
        ]
        .into_iter()
        .filter(|(has, _)| *has)
        .map(|(_, v)| v.to_string())
        .collect();
        let view = if self.view == View::Board {
            "Board"
        } else {
            "Stream"
        }
        .to_string();
        self.view_picker.update(cx, |picker, cx| {
            picker.set_items(views, window, cx);
            picker.set_selected_value(&view, window, cx);
        });
    }

    /// Reads the source list and the shown source's history from the database.
    fn load(&mut self) {
        let loaded = self.store.source_names().and_then(|sources| {
            if !self.source.as_ref().is_some_and(|s| sources.contains(s)) {
                self.source = sources.first().cloned();
            }
            self.sources = sources;
            match &self.source {
                Some(s) => History::load(&self.store, s),
                None => Ok(History::empty()),
            }
        });
        match loaded {
            Ok(h) => {
                self.history = h;
                self.error = None;
            }
            Err(e) => self.error = Some(format!("{e:#}")),
        }
        self.apply_range();
        // A source with only one kind of event has only one view.
        if !self.history.has_states() {
            self.view = View::Stream;
        } else if !self.history.has_moments() {
            self.view = View::Board;
        }
    }

    /// Narrows the history to the chosen dates; a range that is not over yet ends now.
    fn apply_range(&mut self) {
        let now = Local::now().timestamp_millis();
        self.history
            .set_range(self.range.map(|(from, to)| (from, to.min(now).max(from))));
        self.t = self.t.clamp(self.history.start, self.history.end);
    }

    /// Picks up events written by `ithildin mcp` or `ithildin ingest` since the last check.
    fn reload_if_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(version) = self.store.data_version() else {
            return;
        };
        if version == self.seen_version {
            return;
        }
        self.seen_version = version;
        let at_end = self.t >= self.history.end;
        self.load();
        self.sync_pickers(window, cx);
        // Following "now" keeps following it as new events arrive.
        if at_end {
            self.t = self.history.end;
        }
        cx.notify();
    }

    fn show_source(&mut self, source: String, window: &mut Window, cx: &mut Context<Self>) {
        self.source = Some(source);
        self.view = View::Board;
        self.selected = None;
        self.playing = None;
        self.load();
        self.sync_pickers(window, cx);
        self.t = self.history.end;
        cx.notify();
    }

    fn toggle_play(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.playing.take().is_some() {
            cx.notify();
            return;
        }
        if self.t >= self.history.end {
            self.t = self.history.start;
        }
        let step = (self.history.end - self.history.start) as f64
            / (PLAY_SECONDS * 1000.0 / FRAME.as_millis() as f64);
        self.playing = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(FRAME).await;
                let done = this.update(cx, |this, cx| {
                    let mut next = this.t + ((step * this.speed) as i64).max(1);
                    if let Some(event) = this.history.next_event(this.t)
                        && event - this.t > IDLE_SKIP_MS
                    {
                        next = event;
                    }
                    this.t = next.min(this.history.end);
                    cx.notify();
                    this.t >= this.history.end
                });
                if done.unwrap_or(true) {
                    let _ = this.update(cx, |this, cx| {
                        this.playing = None;
                        cx.notify();
                    });
                    break;
                }
            }
        }));
        cx.notify();
    }

    fn seek(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let bar = self.bar.get();
        let width = f32::from(bar.size.width);
        if width <= 0.0 {
            return;
        }
        let f = ((f32::from(x) - f32::from(bar.origin.x)) / width).clamp(0.0, 1.0) as f64;
        self.t = self.history.start + ((self.history.end - self.history.start) as f64 * f) as i64;
        cx.notify();
    }

    /// A group's colour: groups take the hues in the order they first appear in the source.
    fn hue(&self, group: &str) -> &'static Hue {
        &pal().groups[self.history.group_index(group) % pal().groups.len()]
    }

    fn fraction(&self, at: i64) -> f32 {
        let span = (self.history.end - self.history.start).max(1);
        ((at - self.history.start) as f32 / span as f32).clamp(0.0, 1.0)
    }
}

impl Render for Timeline {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(pal().bg))
            .text_color(rgb(pal().text))
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                if this.scrubbing && ev.pressed_button == Some(MouseButton::Left) {
                    this.seek(ev.position.x, cx);
                } else {
                    this.scrubbing = false;
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.scrubbing = false),
            )
            .child(self.header())
            .child(
                div()
                    .flex_1()
                    .flex()
                    .gap(px(10.))
                    .px(px(20.))
                    .overflow_hidden()
                    .child(match self.view {
                        View::Board => self.board(cx).into_any_element(),
                        View::Stream => self.stream(cx).into_any_element(),
                    })
                    .when_some(self.selected.clone(), |el, entity| {
                        el.child(self.panel(&entity, cx))
                    }),
            )
            .child(self.footer(cx))
    }
}

impl Timeline {
    /// Title on the left; on the right what is shown, from broadest to finest: source, dates,
    /// view. Controls for moving in time sit by the timeline (footer).
    fn header(&self) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .px(px(28.))
            .pt(px(22.))
            .pb(px(14.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .truncate()
                            .text_size(px(13.))
                            .text_color(rgb(pal().muted))
                            .child(self.history.title.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(34.))
                            .child(format_time(self.t, "%a %d %b  %H:%M")),
                    ),
            )
            .child(
                div()
                    .w(px(150.))
                    .child(Select::new(&self.source_picker).placeholder("No sources")),
            )
            .child(
                div().w(px(250.)).child(
                    DatePicker::new(&self.range_picker)
                        .presets(presets())
                        .number_of_months(1)
                        .cleanable(true)
                        .placeholder("All dates"),
                ),
            )
            .child(
                div()
                    .w(px(110.))
                    .child(Select::new(&self.view_picker).accessibility_label("View")),
            )
            // Settings are the whole window's: the toolbar's trailing end.
            .child(div().w(px(12.)))
            .child(
                Button::new("settings")
                    .icon(IconName::Settings)
                    .ghost()
                    .tooltip("Settings")
                    .on_click(|_, window, cx| {
                        let settings = cx.new(|cx| crate::settings::Settings::new(window, cx));
                        window.open_dialog(cx, move |dialog, _, _| {
                            dialog.title("Settings").w(px(760.)).child(settings.clone())
                        });
                    }),
            )
    }

    /// The stream's zoom: the span shown, pinched or picked, with ready spans and Reset.
    /// On the board it stays in place, disabled.
    fn zoom_menu(&self, cx: &Context<Self>) -> impl IntoElement {
        div().w(px(100.)).child({
            let this = cx.entity().downgrade();
            let current = self.span;
            Button::new("zoom")
                .label(span_label(self.span))
                .outline()
                .dropdown_caret(true)
                .w_full()
                .disabled(self.view == View::Board)
                .dropdown_menu_with_anchor(Anchor::BottomRight, move |mut menu, _, _| {
                    let set = |span: i64| {
                        let this = this.clone();
                        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                            let _ = this.update(cx, |this, cx| {
                                this.span = span;
                                cx.notify();
                            });
                        }
                    };
                    for &span in SPANS {
                        menu = menu.item(
                            PopupMenuItem::new(span_label(span))
                                .checked(span == current)
                                .on_click(set(span)),
                        );
                    }
                    menu.separator()
                        .item(PopupMenuItem::new("Reset").on_click(set(DEFAULT_SPAN)))
                })
        })
    }

    /// One lane per entity with moments around the playhead; the playhead stays at PLAYHEAD_AT.
    fn stream(&self, cx: &Context<Self>) -> impl IntoElement {
        let span = self.span;
        let from = self.t - (span as f32 * PLAYHEAD_AT) as i64;
        let to = from + span;
        let lanes = self.history.lanes(from, to, &self.expanded);
        let x = move |at: i64| ((at - from) as f32 / span as f32).clamp(0.0, 1.0);
        let empty = lanes.is_empty();
        let measure = self.area.clone();

        div()
            .flex_1()
            .relative()
            .flex()
            .flex_col()
            .p(px(12.))
            .rounded(px(10.))
            .bg(rgb(pal().surface))
            .overflow_hidden()
            // Trackpad pinch: spreading fingers (delta > 0) shows less time, around the playhead.
            .on_pinch(cx.listener(|this, ev: &PinchEvent, _, cx| {
                this.span =
                    ((this.span as f64 / (1.0 + ev.delta as f64)) as i64).clamp(MIN_SPAN, MAX_SPAN);
                cx.notify();
            }))
            // Two-finger sideways swipe moves through time, as the content follows the fingers;
            // vertical swipes are left to the lane list.
            .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _, cx| {
                let d = ev.delta.pixel_delta(px(20.));
                let lanes_width = f32::from(this.area.get().size.width) - 24. - LANE_LABEL;
                if d.x.abs() <= d.y.abs() || lanes_width <= 0. {
                    return;
                }
                this.playing = None;
                let shift = (f32::from(d.x) / lanes_width) as f64 * this.span as f64;
                this.t = (this.t - shift as i64).clamp(this.history.start, this.history.end);
                cx.notify();
            }))
            .child(
                canvas(move |bounds, _, _| measure.set(bounds), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .when_some(self.error.clone(), |el, e| {
                el.child(div().text_color(rgb(pal().error)).child(e))
            })
            .child(
                // Time axis.
                div()
                    .flex()
                    .h(px(24.))
                    .child(div().w(px(LANE_LABEL)).flex_none())
                    .child(
                        div()
                            .flex_1()
                            .relative()
                            .children(ticks(from, to).into_iter().map(|(at, label)| {
                                div()
                                    .absolute()
                                    .top_0()
                                    .bottom_0()
                                    .left(relative(x(at)))
                                    .border_l_1()
                                    .border_color(rgb(pal().border))
                                    .pl(px(4.))
                                    .text_size(px(11.))
                                    .text_color(rgb(pal().muted))
                                    .child(label)
                            }))
                            .child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .bottom_0()
                                    .left(relative(PLAYHEAD_AT))
                                    .w(px(2.))
                                    .bg(rgb(pal().accent)),
                            ),
                    ),
            )
            .when(empty, |el| {
                el.child(
                    div().p(px(12.)).text_color(rgb(pal().muted)).child(
                        "Nothing happens in this window. Zoom out (−) or move the playhead.",
                    ),
                )
            })
            .child(
                div()
                    .id("lanes")
                    .flex_1()
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .relative()
                            .flex()
                            .flex_col()
                            .child(self.forks(&lanes, &x))
                            .children({
                                // Child lanes take a tone per parent, in order of the parents on screen.
                                let mut parents: Vec<&str> = vec![];
                                lanes
                                    .iter()
                                    .map(|lane| {
                                        let hue = match self
                                            .history
                                            .parent_of(lane.entity)
                                            .filter(|_| lane.depth > 0)
                                        {
                                            Some(p) => {
                                                let i = parents
                                                    .iter()
                                                    .position(|q| *q == p)
                                                    .unwrap_or_else(|| {
                                                        parents.push(p);
                                                        parents.len() - 1
                                                    });
                                                &pal().children[i % pal().children.len()]
                                            }
                                            None => self.hue(lane.group),
                                        };
                                        self.lane(lane, hue, &x, cx)
                                    })
                                    .collect::<Vec<_>>()
                            }),
                    ),
            )
    }

    /// Thin dashed elbows from each open parent to its child lanes: down from the parent's last
    /// moment before the child started, then right to the child's first moment.
    fn forks(&self, lanes: &[Lane], x: &impl Fn(i64) -> f32) -> Div {
        let mid = |row: usize| row as f32 * LANE_HEIGHT + LANE_HEIGHT / 2.;
        let line = || {
            div()
                .absolute()
                .border_dashed()
                .border_color(rgba((pal().muted << 8) | 0x90))
        };
        let mut parents: Vec<usize> = vec![]; // the open lane at each depth above the current row
        let mut lines = vec![];
        for (row, lane) in lanes.iter().enumerate() {
            parents.truncate(lane.depth);
            if let (Some(&parent), Some((from, start))) =
                (parents.last(), self.history.fork(lane.entity))
            {
                let (xa, xc) = (x(from), x(start));
                lines.push(
                    line()
                        .left(relative(xa))
                        .top(px(mid(parent)))
                        .h(px(mid(row) - mid(parent)))
                        .border_l_1(),
                );
                lines.push(
                    line()
                        .left(relative(xa))
                        .top(px(mid(row)))
                        .w(relative((xc - xa).max(0.)))
                        .border_t_1(),
                );
            }
            parents.push(row);
        }
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .left(px(LANE_LABEL))
            .right_0()
            .children(lines)
    }

    fn lane(&self, lane: &Lane, hue: &Hue, x: &impl Fn(i64) -> f32, cx: &Context<Self>) -> Div {
        let selected = self.selected.as_deref() == Some(lane.entity);
        let span = self.span;
        let select = |entity: &str| {
            let entity = entity.to_string();
            cx.listener(move |this: &mut Self, _: &MouseDownEvent, _, cx| {
                this.selected = Some(entity.clone());
                cx.notify();
            })
        };
        div()
            .flex()
            .h(px(LANE_HEIGHT))
            .items_center()
            .border_b_1()
            .border_color(rgb(pal().border_subtle))
            .child(
                div()
                    .w(px(LANE_LABEL))
                    .flex_none()
                    .flex()
                    .items_center()
                    .pl(px(14. * lane.depth as f32))
                    .pr(px(8.))
                    .text_size(px(12.))
                    .child({
                        // A lane with child lanes opens and closes them; the count shows while closed.
                        let open = self.expanded.contains(lane.entity);
                        let entity = lane.entity.to_string();
                        div()
                            .flex_none()
                            .w(px(14.))
                            .text_color(rgb(pal().muted))
                            .when(lane.children > 0, |el| {
                                el.cursor_pointer()
                                    .child(if open { "▾" } else { "▸" })
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, _, cx| {
                                            if !this.expanded.remove(&entity) {
                                                this.expanded.insert(entity.clone());
                                            }
                                            cx.stop_propagation();
                                            cx.notify();
                                        }),
                                    )
                            })
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(rgb(if selected {
                                pal().accent
                            } else if lane.depth > 0 {
                                pal().muted
                            } else {
                                pal().text
                            }))
                            .cursor_pointer()
                            .on_mouse_down(MouseButton::Left, select(lane.entity))
                            .child(one_line(lane.label)),
                    )
                    .when(
                        lane.children > 0 && !self.expanded.contains(lane.entity),
                        |el| {
                            el.child(
                                div()
                                    .flex_none()
                                    .pl(px(4.))
                                    .text_size(px(10.))
                                    .text_color(rgb(pal().muted))
                                    .child(lane.children.to_string()),
                            )
                        },
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .relative()
                    .children(lane.moments.iter().map(|e| {
                        let ago = self.t - e.at;
                        // Just happened: brighter and larger, fading over 5% of the window.
                        let glow = if ago >= 0 {
                            (1.0 - ago as f32 / (span as f32 * 0.05)).clamp(0.0, 1.0)
                        } else {
                            0.0
                        };
                        let size = 9. + 6. * glow;
                        let fill = if ago < 0 {
                            mix(hue.solid, pal().bg, 0.65)
                        } else {
                            mix(hue.solid, 0xffffff, glow * 0.5)
                        };
                        // A narrow card: time and kind, the lane's title, the first 100 characters of
                        // the content; long prompts wrap instead of filling the screen.
                        let head =
                            format!("{}  ·  {}", format_time(e.at, "%a %d %b %H:%M"), e.kind);
                        let title = shorten(&one_line(lane.label), 80);
                        let preview = shorten(&one_line(&summary(e)), 100);
                        div()
                            .id(SharedString::from(format!("{}@{}", e.entity, e.at)))
                            .absolute()
                            .top(px(13. - size / 2.))
                            .left(relative(x(e.at)))
                            .size(px(size))
                            .rounded_full()
                            .bg(rgb(fill))
                            .cursor_pointer()
                            .tooltip(move |window, cx| {
                                let (head, title, preview) =
                                    (head.clone(), title.clone(), preview.clone());
                                Tooltip::element(move |_, _| {
                                    div()
                                        .w(px(260.))
                                        .py(px(4.))
                                        .flex()
                                        .flex_col()
                                        .gap(px(3.))
                                        .child(
                                            div()
                                                .text_size(px(11.))
                                                .text_color(rgb(pal().muted))
                                                .child(head.clone()),
                                        )
                                        .child(div().child(title.clone()))
                                        .when(!preview.is_empty() && preview != title, |el| {
                                            el.child(
                                                div()
                                                    .text_size(px(12.))
                                                    .text_color(rgb(pal().muted))
                                                    .child(preview.clone()),
                                            )
                                        })
                                })
                                .build(window, cx)
                            })
                            .tooltip_show_delay(Duration::from_millis(120))
                            .on_mouse_down(MouseButton::Left, select(&e.entity))
                    }))
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left(relative(PLAYHEAD_AT))
                            .w(px(2.))
                            .bg(rgba((pal().accent << 8) | 0x60)),
                    ),
            )
    }

    fn board(&self, cx: &Context<Self>) -> impl IntoElement {
        let columns = self.history.board_at(self.t);
        let empty = self.sources.is_empty();
        let area = self.area.get();
        let n = columns.len().max(1) as f32;
        // Inside a column: 12 px padding each side, a 20 px header and a 10 px gap under it.
        let column_width = (f32::from(area.size.width) - 10. * (n - 1.)) / n - 24.;
        let column_height = f32::from(area.size.height) - 24. - 30.;
        let measure = self.area.clone();
        div()
            .flex_1()
            .relative()
            .flex()
            .gap(px(10.))
            .overflow_hidden()
            .child(canvas(move |bounds, _, _| measure.set(bounds), |_, _, _, _| {}).absolute().size_full())
            .when_some(self.error.clone(), |el, e| el.child(div().text_color(rgb(pal().error)).child(e)))
            .when(empty && self.error.is_none(), |el| {
                el.child(div().p(px(12.)).text_color(rgb(pal().muted)).child(
                    "No state events yet. Feed a source with `ithildin ingest` (JSON Lines) or the MCP tool add_event.",
                ))
            })
            .children(columns.into_iter().map(|col| {
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .p(px(12.))
                    .rounded(px(10.))
                    .bg(rgb(pal().surface))
                    .overflow_hidden()
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .text_size(px(14.))
                            .text_color(rgb(pal().muted))
                            .child(col.status.clone())
                            .child(col.cards.len().to_string()),
                    )
                    .child({
                        let (size, scroll) = fit(col.cards.len(), column_width, column_height);
                        div()
                            .id(SharedString::from(format!("column-{}", col.status)))
                            .flex_1()
                            .flex()
                            .flex_wrap()
                            .content_start()
                            .gap(px(CARD_GAP))
                            .when(scroll, |el| el.overflow_y_scroll())
                            .children(col.cards.iter().map(|c| self.card(c, size, cx)))
                    })
            }))
    }

    fn card(&self, c: &Card, (width, height, text): (f32, f32, f32), cx: &Context<Self>) -> Div {
        let hue = self.hue(&c.group);
        let selected = self.selected.as_deref() == Some(c.entity.as_str());
        let entity = c.entity.clone();
        div()
            .w(px(width))
            .h(px(height))
            .flex()
            .items_center()
            .px(px(if width < 70. { 5. } else { 8. }))
            .rounded(px(6.))
            .border_1()
            .border_color(rgb(if selected {
                pal().focus
            } else {
                mix(hue.border, hue.solid, c.glow)
            }))
            .when(selected, |el| el.border_2())
            // Just worked on: the background warms toward the border step, the border to solid.
            .bg(rgb(mix(hue.bg, hue.border, c.glow * 0.8)))
            .text_color(rgb(hue.text))
            .text_size(px(text))
            .overflow_hidden()
            .whitespace_nowrap()
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.selected = Some(entity.clone());
                    cx.notify();
                }),
            )
            .child(c.label.clone())
    }

    /// The clicked entity at the playhead: its links, its metadata, and what happened to it so far.
    fn panel(&self, entity: &str, cx: &Context<Self>) -> impl IntoElement {
        let state = self.history.state_of(entity, self.t);
        let title = shorten(&one_line(self.history.label_of(entity)), 120);
        let links = self.history.links_of(entity);
        let meta: Vec<(String, String)> = state
            .and_then(|e| e.data.as_object())
            .into_iter()
            .flatten()
            .filter(|(k, _)| !SHOWN_FIELDS.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), value_text(v)))
            .collect();
        let status = match state {
            Some(e) => format!(
                "{} · since {}",
                field(e, "to").unwrap_or("?"),
                format_time(e.at, "%a %d %b %H:%M")
            ),
            None => "not there yet at this moment".into(),
        };

        div()
            .id("panel")
            .w(px(PANEL_WIDTH))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(14.))
            .p(px(16.))
            .rounded(px(10.))
            .bg(rgb(pal().surface))
            .border_1()
            .border_color(rgb(pal().border))
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .justify_between()
                    .gap(px(8.))
                    // The title column shrinks and wraps, so a long title never pushes the close button out.
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().text_size(px(22.)).child(title))
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(rgb(pal().muted))
                                    .child(format!("{entity} · {status}")),
                            ),
                    )
                    .child(
                        div().flex_none().child(
                            Button::new("close-panel")
                                .icon(IconName::Close)
                                .ghost()
                                .tooltip("Close")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.selected = None;
                                    cx.notify();
                                })),
                        ),
                    ),
            )
            .when(!links.is_empty(), |el| {
                el.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(8.))
                        .children(links.into_iter().map(|link| {
                            button(format!("{}  ↗", link.label), false)
                                .on_mouse_down(MouseButton::Left, move |_, _, cx| open(&link, cx))
                        })),
                )
            })
            .when(!meta.is_empty(), |el| {
                el.child(section("Metadata"))
                    .children(meta.into_iter().map(|(k, v)| {
                        let link = as_link(&v);
                        div()
                            .flex()
                            .gap(px(10.))
                            .text_size(px(13.))
                            .child(
                                div()
                                    .w(px(90.))
                                    .flex_none()
                                    .text_color(rgb(pal().muted))
                                    .child(k),
                            )
                            .child(match link {
                                Some(url) => div()
                                    .text_color(rgb(pal().accent))
                                    .cursor_pointer()
                                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                        cx.open_url(&url)
                                    })
                                    .child(v),
                                None => div().child(v),
                            })
                    }))
            })
            .child(section("History"))
            .children(
                self.history
                    .events_of(entity, self.t, HISTORY_ROWS)
                    .into_iter()
                    .map(|e| {
                        div()
                            .flex()
                            .gap(px(10.))
                            .text_size(px(12.))
                            .child(
                                div()
                                    .w(px(90.))
                                    .flex_none()
                                    .text_color(rgb(pal().muted))
                                    .child(format_time(e.at, "%a %d %H:%M")),
                            )
                            .child(
                                div()
                                    .w(px(60.))
                                    .flex_none()
                                    .text_color(rgb(pal().muted))
                                    .child(e.kind.clone()),
                            )
                            .child(div().flex_1().min_w_0().truncate().child(summary(e)))
                    }),
            )
    }

    fn footer(&self, cx: &Context<Self>) -> impl IntoElement {
        let play = if self.playing.is_some() {
            "❚❚  Pause"
        } else {
            "▶  Play"
        };
        // The last move before the playhead, like a subtitle; a click opens its entity.
        let caption = self.history.last_change(self.t).map(|e| {
            let entity = e.entity.clone();
            div()
                .flex()
                .gap(px(10.))
                .min_w_0()
                .cursor_pointer()
                .hover(|s| s.text_color(rgb(pal().text)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        this.selected = Some(entity.clone());
                        cx.notify();
                    }),
                )
                .child(div().flex_none().text_size(px(11.)).child("LAST MOVE"))
                .child(div().truncate().child(format!(
                    "{}   {} → {}   {}",
                    field(e, "label").unwrap_or(&e.entity),
                    field(e, "from").unwrap_or("?"),
                    field(e, "to").unwrap_or("?"),
                    format_time(e.at, "%a %H:%M")
                )))
        });
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .px(px(28.))
            .pt(px(14.))
            .pb(px(22.))
            // Everything about moving in time sits with the timeline: playback, speed, zoom.
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .w(px(96.))
                            .flex()
                            .justify_center()
                            .child(button(play.into(), self.playing.is_some()))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, window, cx| this.toggle_play(window, cx)),
                            ),
                    )
                    .child(div().w(px(84.)).child(
                        Select::new(&self.speed_picker).accessibility_label("Playback speed"),
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .text_size(px(14.))
                            .text_color(rgb(pal().muted))
                            .children(caption),
                    )
                    .child(self.zoom_menu(cx)),
            )
            .child(self.bar_view(cx))
    }

    /// Event histogram with day marks and the playhead; press and drag to move in time.
    fn bar_view(&self, cx: &Context<Self>) -> impl IntoElement {
        let counts = self.history.density(BUCKETS);
        let peak = counts.iter().copied().max().unwrap_or(1).max(1) as f32;
        let bar = self.bar.clone();
        let playhead = self.fraction(self.t);

        div()
            .relative()
            .h(px(72.))
            .rounded(px(8.))
            .bg(rgb(pal().surface))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                    this.playing = None;
                    this.scrubbing = true;
                    this.seek(ev.position.x, cx);
                }),
            )
            // Records the bar's bounds each frame for `seek`.
            .child(
                canvas(move |bounds, _, _| bar.set(bounds), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .child(div().absolute().size_full().flex().items_end().children(
                counts.into_iter().enumerate().map(move |(i, n)| {
                    let past = (i as f32 + 0.5) / BUCKETS as f32 <= playhead;
                    div()
                        .flex_1()
                        .h(relative(n as f32 / peak * 0.85))
                        .bg(rgba(if past {
                            (pal().accent << 8) | 0xb0
                        } else {
                            (pal().muted << 8) | 0x50
                        }))
                }),
            ))
            .children(self.day_marks())
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(relative(playhead))
                    .w(px(2.))
                    .bg(rgb(pal().accent)),
            )
    }

    fn day_marks(&self) -> Vec<Div> {
        let Some(mut day) = Local.timestamp_millis_opt(self.history.start).single() else {
            return vec![];
        };
        let mut marks = vec![];
        loop {
            let next = (day.date_naive() + chrono::Days::new(1))
                .and_hms_opt(0, 0, 0)
                .and_then(|d| Local.from_local_datetime(&d).earliest());
            let Some(next) = next else { break };
            if next.timestamp_millis() > self.history.end {
                break;
            }
            marks.push(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(relative(self.fraction(next.timestamp_millis())))
                    .border_l_1()
                    .border_color(rgb(pal().border))
                    .pl(px(4.))
                    .text_size(px(11.))
                    .text_color(rgb(pal().muted))
                    .child(next.format("%a %d").to_string()),
            );
            day = next;
        }
        marks
    }
}

fn today() -> NaiveDate {
    Local::now().date_naive()
}

/// The week `back` weeks before the one holding `day`: Monday to Sunday.
fn week(day: NaiveDate, back: u64) -> (NaiveDate, NaiveDate) {
    let monday = day - Days::new(day.weekday().num_days_from_monday() as u64) - Days::new(7 * back);
    (monday, monday + Days::new(6))
}

/// Local midnight at the start of `day`, in unix ms.
fn day_start(day: NaiveDate) -> i64 {
    day.and_hms_opt(0, 0, 0)
        .and_then(|d| Local.from_local_datetime(&d).earliest())
        .map_or(0, |t| t.timestamp_millis())
}

fn presets() -> Vec<DateRangePreset> {
    let today = today();
    let (this_start, this_end) = week(today, 0);
    let (last_start, last_end) = week(today, 1);
    vec![
        DateRangePreset::range("This week", this_start, this_end),
        DateRangePreset::range("Last week", last_start, last_end),
        DateRangePreset::range("Last 2 weeks", last_start, this_end),
        DateRangePreset::range("Last 30 days", today - Days::new(29), today),
    ]
}

/// The largest card size at which `cards` fit a column of `width` × `height` px, and whether
/// the column must scroll anyway (it does at the smallest size if they still do not fit).
fn fit(cards: usize, width: f32, height: f32) -> ((f32, f32, f32), bool) {
    let fits = |&(w, h, _): &(f32, f32, f32)| {
        let per_row = ((width + CARD_GAP) / (w + CARD_GAP)).floor().max(1.);
        let rows = (cards as f32 / per_row).ceil();
        rows * (h + CARD_GAP) - CARD_GAP <= height
    };
    match CARD_SIZES.iter().find(|s| fits(s)) {
        Some(&size) => (size, false),
        None => (CARD_SIZES[CARD_SIZES.len() - 1], true),
    }
}

/// Axis marks for [from, to]: about six of them, on round local times.
fn ticks(from: i64, to: i64) -> Vec<(i64, String)> {
    const STEPS: &[i64] = &[
        MINUTE,
        5 * MINUTE,
        15 * MINUTE,
        HOUR,
        3 * HOUR,
        6 * HOUR,
        12 * HOUR,
        24 * HOUR,
    ];
    let step = STEPS
        .iter()
        .copied()
        .find(|s| (to - from) / s <= 8)
        .unwrap_or(24 * HOUR);
    let fmt = if step >= 24 * HOUR {
        "%a %d"
    } else {
        "%a %H:%M"
    };
    // Round to local time: shift by the zone offset, floor to the step, shift back.
    let offset = Local
        .timestamp_millis_opt(from)
        .single()
        .map_or(0, |t| t.offset().local_minus_utc() as i64 * 1000);
    let mut at = (from + offset).div_euclid(step) * step - offset + step;
    let mut marks = vec![];
    while at < to {
        marks.push((at, format_time(at, fmt)));
        at += step;
    }
    marks
}

fn span_label(ms: i64) -> String {
    if ms < HOUR {
        format!("{} min", (ms + MINUTE / 2) / MINUTE)
    } else if ms < 48 * HOUR {
        format!("{} h", (ms + HOUR / 2) / HOUR)
    } else {
        format!("{} d", (ms + 12 * HOUR) / (24 * HOUR))
    }
}

/// Opens a link: a file:// link with an `app` in that app (`open -a`), anything else as macOS does.
fn open(link: &Opened, cx: &mut App) {
    match (&link.app, link.url.strip_prefix("file://")) {
        (Some(app), Some(path)) => {
            if let Err(e) = std::process::Command::new("open")
                .args(["-a", app, &path.replace("%20", " ")])
                .spawn()
            {
                eprintln!("open {} in {app}: {e}", link.url);
            }
        }
        _ => cx.open_url(&link.url),
    }
}

fn button(label: String, active: bool) -> Div {
    div()
        .px(px(14.))
        .py(px(7.))
        .rounded(px(8.))
        .bg(rgb(if active {
            pal().selected
        } else {
            pal().surface
        }))
        .border_1()
        .border_color(rgb(if active { pal().focus } else { pal().border }))
        .text_size(px(13.))
        .cursor_pointer()
        .when(!active, |el| el.hover(|s| s.bg(rgb(pal().hover))))
        .child(label)
}

fn section(title: &str) -> Div {
    div()
        .pt(px(4.))
        .text_size(px(11.))
        .text_color(rgb(pal().muted))
        .child(title.to_uppercase())
}

/// One line for an event in the panel's history.
/// Text with its line breaks and runs of spaces as single spaces, for one-line places.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The first `n` characters, with an ellipsis when cut.
fn shorten(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", s[..i].trim_end()),
        None => s.to_string(),
    }
}

fn summary(e: &Event) -> String {
    if e.kind == "state" {
        return format!(
            "{} → {}",
            field(e, "from").unwrap_or("·"),
            field(e, "to").unwrap_or("?")
        );
    }
    let mut data = e.data.clone();
    if let Some(o) = data.as_object_mut() {
        o.retain(|k, _| !SHOWN_FIELDS.contains(&k.as_str()));
    }
    match field(e, "subject") {
        Some(s) => s.to_string(),
        None => value_text(&data),
    }
}

fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// A metadata value worth opening: a web or file URL, or an absolute path on this Mac.
fn as_link(v: &str) -> Option<String> {
    if v.starts_with("http://") || v.starts_with("https://") || v.starts_with("file://") {
        Some(v.to_string())
    } else if v.starts_with('/') && std::path::Path::new(v).exists() {
        Some(format!("file://{}", v.replace(' ', "%20")))
    } else {
        None
    }
}

fn mix(a: u32, b: u32, f: f32) -> u32 {
    let ch = |shift: u32| {
        let (x, y) = (((a >> shift) & 0xff) as f32, ((b >> shift) & 0xff) as f32);
        ((x + (y - x) * f).round() as u32) << shift
    };
    ch(16) | ch(8) | ch(0)
}

fn format_time(ms: i64, fmt: &str) -> String {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format(fmt).to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's `test` macro would shadow the standard one.
    use super::{CARD_SIZES, HOUR, MINUTE, fit, shorten, ticks, week};
    use chrono::{Local, TimeZone as _, Timelike as _};

    #[test]
    fn weeks_run_monday_to_sunday() {
        let friday = chrono::NaiveDate::from_ymd_opt(2026, 10, 16).unwrap();
        let d = |m, day| chrono::NaiveDate::from_ymd_opt(2026, m, day).unwrap();
        assert_eq!(week(friday, 0), (d(10, 12), d(10, 18)));
        assert_eq!(week(friday, 1), (d(10, 5), d(10, 11)));
        assert_eq!(
            week(d(10, 12), 0),
            (d(10, 12), d(10, 18)),
            "a Monday starts its own week"
        );
    }

    #[test]
    fn cards_shrink_before_the_column_scrolls() {
        // 115 px wide holds one card per row at every size. 3 roomy cards: 3 × 32 − 6 = 90 px.
        assert_eq!(fit(3, 115., 200.), (CARD_SIZES[0], false));
        // 21 cards in 400 px: roomy 666, medium 582, small 498 px — none fits, so small and scroll.
        assert_eq!(fit(21, 115., 400.), (CARD_SIZES[2], true));
        // 160 px wide holds two medium cards per row: 11 rows × 28 − 6 = 302 px.
        assert_eq!(fit(21, 160., 320.), (CARD_SIZES[1], false));
    }

    #[test]
    fn shorten_cuts_on_characters() {
        assert_eq!(shorten("şirin", 3), "şir…");
        assert_eq!(shorten("kısa", 10), "kısa");
    }

    #[test]
    fn ticks_fall_on_round_local_times_inside_the_window() {
        let from = Local
            .with_ymd_and_hms(2026, 10, 14, 9, 37, 0)
            .unwrap()
            .timestamp_millis();
        let marks = ticks(from, from + 6 * HOUR);
        assert!(
            !marks.is_empty() && marks.len() <= 8,
            "{} marks",
            marks.len()
        );
        for (at, _) in &marks {
            let t = Local.timestamp_millis_opt(*at).unwrap();
            assert!(*at > from && *at < from + 6 * HOUR);
            assert_eq!((t.minute(), t.second()), (0, 0), "{t}");
        }
        assert_eq!(marks[0].1, "Wed 10:00");
        // The finest zoom still gets marks, a minute apart.
        let fine = ticks(from, from + 5 * MINUTE);
        assert_eq!(
            fine.iter().map(|(_, l)| l.as_str()).collect::<Vec<_>>(),
            ["Wed 09:38", "Wed 09:39", "Wed 09:40", "Wed 09:41"]
        );
    }
}

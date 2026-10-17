//! A source's events held in memory, so the window can answer "what was true at t" every frame.
//! Knows only contract v1: `state` events place entities in columns, every other kind is a moment.

use std::collections::{HashMap, HashSet};

use anyhow::Result;

use crate::sources::{self, Link, Opened};
use crate::store::{Event, Store};

/// How long a card keeps glowing after a moment on its entity, in ms of timeline time.
const GLOW_MS: i64 = 2 * 3600 * 1000;

pub struct History {
    pub title: String,
    /// Board column order from the source description; other values follow by first appearance.
    columns: Vec<String>,
    /// `state` events, oldest first.
    states: Vec<Event>,
    /// Every other event, oldest first.
    moments: Vec<Event>,
    /// The span the window shows: the date range when one is set, else all the data.
    pub start: i64,
    pub end: i64,
    data_start: i64,
    data_end: i64,
    /// With a date range: the entities that have an event inside it; the others are hidden.
    visible: Option<HashSet<String>>,
    /// Link templates from the source's config, and the values they may use besides the entity's own.
    links: Vec<Link>,
    link_values: HashMap<String, String>,
    /// Entity → position of its first event in the source; fixes the lane order.
    first_seen: HashMap<String, usize>,
    /// Entity → its newest `label`, whichever event carried it.
    labels: HashMap<String, String>,
    /// Group → its place in order of first appearance; gives each group its own colour.
    groups: HashMap<String, usize>,
    /// Entity → its newest `parent`: the stream nests its lane under the parent's.
    parents: HashMap<String, String>,
}

pub struct Card {
    pub entity: String,
    pub label: String,
    pub group: String,
    /// 1 right after a moment on the entity, fading to 0 over `GLOW_MS`.
    pub glow: f32,
}

/// One entity's moments inside the stream view's window.
pub struct Lane<'a> {
    pub entity: &'a str,
    pub label: &'a str,
    pub group: &'a str,
    pub moments: Vec<&'a Event>,
    /// How many parents up to a top lane.
    pub depth: usize,
    /// Child lanes in the window, shown only when this lane is expanded.
    pub children: usize,
}

pub struct Column {
    pub status: String,
    pub cards: Vec<Card>,
}

impl History {
    pub fn load(store: &Store, source: &str) -> Result<Self> {
        let events = store.between(source, None, None, i64::MIN, i64::MAX, i64::MAX)?;
        let description = store.description(source)?;
        let title = description
            .as_ref()
            .and_then(|d| d.title.clone())
            .unwrap_or_else(|| source.to_string());
        let columns = description.map(|d| d.columns).unwrap_or_default();
        let mut h = Self::from_events(title, columns, events);
        if let Some(config) = store.config(source)? {
            h.links = sources::links(&config)?;
            h.link_values = sources::template_values(&config);
        }
        Ok(h)
    }

    pub fn empty() -> Self {
        Self::from_events(String::new(), vec![], vec![])
    }

    fn from_events(title: String, mut columns: Vec<String>, events: Vec<Event>) -> Self {
        let start = events.first().map_or(0, |e| e.at);
        let end = events.last().map_or(0, |e| e.at);
        let (states, moments): (Vec<_>, Vec<_>) =
            events.into_iter().partition(|e| e.kind == "state");
        for e in &states {
            let to = to(e);
            if !columns.iter().any(|c| c == to) {
                columns.push(to.to_string());
            }
        }
        let mut first_seen = HashMap::new();
        let mut labels = HashMap::new();
        let mut groups = HashMap::new();
        let mut parents = HashMap::new();
        let mut all: Vec<&Event> = states.iter().chain(&moments).collect();
        all.sort_by_key(|e| e.at);
        for e in all {
            let next = first_seen.len();
            first_seen.entry(e.entity.clone()).or_insert(next);
            if let Some(label) = field(e, "label") {
                labels.insert(e.entity.clone(), label.to_string());
            }
            if let Some(parent) = field(e, "parent") {
                parents.insert(e.entity.clone(), parent.to_string());
            }
            if let Some(group) = field(e, "group") {
                let next = groups.len();
                groups.entry(group.to_string()).or_insert(next);
            }
        }
        History {
            title,
            columns,
            states,
            moments,
            start,
            end,
            data_start: start,
            data_end: end,
            visible: None,
            links: vec![],
            link_values: HashMap::new(),
            first_seen,
            labels,
            groups,
            parents,
        }
    }

    /// Shows only [from, to] and the entities with an event in it; None shows everything.
    /// An entity's state still comes from its whole history, so a task that went `active`
    /// before the range and was worked on inside it sits in `active`.
    pub fn set_range(&mut self, range: Option<(i64, i64)>) {
        match range {
            None => {
                (self.start, self.end, self.visible) = (self.data_start, self.data_end, None);
            }
            Some((from, to)) => {
                let inside = self
                    .states
                    .iter()
                    .chain(&self.moments)
                    .filter(|e| e.at >= from && e.at <= to);
                self.visible = Some(inside.map(|e| e.entity.clone()).collect());
                (self.start, self.end) = (from, to.max(from));
            }
        }
    }

    fn shown(&self, entity: &str) -> bool {
        self.visible.as_ref().is_none_or(|v| v.contains(entity))
    }

    /// The board at `t`: every entity in the column of its latest state at or before `t`.
    pub fn board_at(&self, t: i64) -> Vec<Column> {
        let upto = self.states.partition_point(|e| e.at <= t);
        let mut latest: HashMap<&str, &Event> = HashMap::new();
        for e in &self.states[..upto] {
            latest.insert(&e.entity, e);
        }
        let last_moment = self.last_moments(t);

        let mut columns: Vec<Column> = self
            .columns
            .iter()
            .map(|s| Column {
                status: s.clone(),
                cards: vec![],
            })
            .collect();
        let mut entities: Vec<_> = latest.into_values().collect();
        entities.sort_by(|a, b| a.entity.cmp(&b.entity));
        for e in entities.into_iter().filter(|e| self.shown(&e.entity)) {
            let since = last_moment
                .get(e.entity.as_str())
                .map_or(i64::MAX, |m| t - m);
            let Some(column) = columns.iter_mut().find(|c| c.status == to(e)) else {
                continue;
            };
            column.cards.push(Card {
                entity: e.entity.clone(),
                label: field(e, "label").unwrap_or(&e.entity).to_string(),
                group: field(e, "group").unwrap_or_default().to_string(),
                glow: (1.0 - since as f32 / GLOW_MS as f32).clamp(0.0, 1.0),
            });
        }
        columns
    }

    /// Latest state change at or before `t`, for the caption under the board.
    pub fn last_change(&self, t: i64) -> Option<&Event> {
        let upto = self.states.partition_point(|e| e.at <= t);
        self.states[..upto]
            .iter()
            .rev()
            .find(|e| !e.data["from"].is_null())
    }

    /// The entity's latest state at or before `t`: its data is the metadata the panel shows.
    pub fn state_of(&self, entity: &str, t: i64) -> Option<&Event> {
        let upto = self.states.partition_point(|e| e.at <= t);
        self.states[..upto]
            .iter()
            .rev()
            .find(|e| e.entity == entity)
    }

    /// Links for the entity: the source's templates filled with its values (each data field's
    /// newest value across all its events — where it is now, whatever the playhead), then any
    /// `data.links` its events carry.
    pub fn links_of(&self, entity: &str) -> Vec<Opened> {
        let mut events: Vec<&Event> = self
            .states
            .iter()
            .chain(&self.moments)
            .filter(|e| e.entity == entity)
            .collect();
        events.sort_by_key(|e| e.at);
        let mut values = self.link_values.clone();
        values.insert("entity".into(), entity.into());
        for e in &events {
            for (k, v) in e.data.as_object().into_iter().flatten() {
                if let Some(text) = v.as_str() {
                    values.insert(k.clone(), text.to_string());
                }
            }
        }
        // A state event names its state `to`; templates call it `state`.
        if let Some(to) = self.state_of(entity, i64::MAX).and_then(|e| field(e, "to")) {
            values.insert("state".into(), to.to_string());
        }
        let mut links = sources::resolve(&self.links, &values);
        let mut seen: HashSet<String> = links.iter().map(|o| o.url.clone()).collect();
        for e in events.iter().rev() {
            for l in e.data["links"].as_array().into_iter().flatten() {
                let Some(url) = l["url"].as_str() else {
                    continue;
                };
                if seen.insert(url.to_string()) {
                    links.push(Opened {
                        label: l["label"].as_str().unwrap_or(url).to_string(),
                        url: url.to_string(),
                        app: l["app"].as_str().map(str::to_string),
                    });
                }
            }
        }
        links
    }

    /// The entity's events at or before `t`, newest first.
    pub fn events_of(&self, entity: &str, t: i64, limit: usize) -> Vec<&Event> {
        let mut all: Vec<&Event> = self
            .states
            .iter()
            .chain(&self.moments)
            .filter(|e| e.entity == entity && e.at <= t)
            .collect();
        all.sort_by_key(|e| std::cmp::Reverse(e.at));
        all.truncate(limit);
        all
    }

    /// Events per bucket across [start, end], for the bar under the timeline.
    pub fn density(&self, buckets: usize) -> Vec<u32> {
        let mut counts = vec![0; buckets];
        let span = (self.end - self.start).max(1) as f64;
        for e in self
            .states
            .iter()
            .chain(&self.moments)
            .filter(|e| e.at >= self.start && e.at <= self.end)
        {
            let i = ((e.at - self.start) as f64 / span * buckets as f64) as usize;
            counts[i.min(buckets - 1)] += 1;
        }
        counts
    }

    /// The next moment after `t` where anything happens; used to skip idle hours during playback.
    pub fn next_event(&self, t: i64) -> Option<i64> {
        let next = |v: &[Event]| v.get(v.partition_point(|e| e.at <= t)).map(|e| e.at);
        [next(&self.states), next(&self.moments)]
            .into_iter()
            .flatten()
            .min()
    }

    /// The group's place in order of first appearance (0 for an unknown or empty group).
    pub fn group_index(&self, group: &str) -> usize {
        self.groups.get(group).copied().unwrap_or(0)
    }

    pub fn has_states(&self) -> bool {
        !self.states.is_empty()
    }

    pub fn has_moments(&self) -> bool {
        !self.moments.is_empty()
    }

    /// Lanes for the moments in [from, to]: one per entity that has a moment there, in the order
    /// entities first appeared, so lanes do not jump around during playback. A lane with a
    /// `parent` sits under its parent's lane (added empty when the parent has no moment there),
    /// and only while the parent is in `expanded`.
    pub fn lanes(&self, from: i64, to: i64, expanded: &HashSet<String>) -> Vec<Lane<'_>> {
        let lo = self.moments.partition_point(|e| e.at < from);
        let hi = self.moments.partition_point(|e| e.at <= to);
        let mut lanes: HashMap<&str, Lane> = HashMap::new();
        for e in self.moments[lo..hi]
            .iter()
            .filter(|e| self.shown(&e.entity))
        {
            lanes
                .entry(&e.entity)
                .or_insert_with(|| self.lane(&e.entity, field(e, "group").unwrap_or_default()))
                .moments
                .push(e);
        }
        // Every ancestor of a lane gets a lane too; a parent loop stops after a few steps.
        let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
        let mut roots = vec![];
        let mut todo: Vec<&str> = lanes.keys().copied().collect();
        while let Some(entity) = todo.pop() {
            match self.parent_of(entity) {
                Some(parent) if !self.is_ancestor(entity, parent) => {
                    if !lanes.contains_key(parent) {
                        let group = lanes[entity].group;
                        lanes.insert(parent, self.lane(parent, group));
                        todo.push(parent);
                    }
                    children.entry(parent).or_default().push(entity);
                }
                _ => roots.push(entity),
            }
        }
        let order = |v: &mut Vec<&str>| {
            v.sort_by_key(|e| self.first_seen.get(*e).copied().unwrap_or(usize::MAX))
        };
        order(&mut roots);
        children.values_mut().for_each(order);

        let mut out = Vec::with_capacity(lanes.len());
        let mut stack: Vec<(&str, usize)> = roots.into_iter().rev().map(|e| (e, 0)).collect();
        while let Some((entity, depth)) = stack.pop() {
            let mut lane = lanes.remove(entity).expect("every entity has a lane");
            let kids = children.get(entity).map_or(&[][..], Vec::as_slice);
            (lane.depth, lane.children) = (depth, kids.len());
            if expanded.contains(entity) {
                stack.extend(kids.iter().rev().map(|k| (*k, depth + 1)));
            }
            out.push(lane);
        }
        out
    }

    /// Where a child lane forks off its parent: the parent's last moment at or before the child's
    /// first one (else the child's start), and the child's start. Times are ms.
    pub fn fork(&self, child: &str) -> Option<(i64, i64)> {
        let parent = self.parent_of(child)?;
        // ponytail: linear scans per child lane and frame; index moments by entity if lanes grow.
        let start = self.moments.iter().find(|e| e.entity == child)?.at;
        let from = self
            .moments
            .iter()
            .take_while(|e| e.at <= start)
            .filter(|e| e.entity == parent)
            .last()
            .map_or(start, |e| e.at);
        Some((from, start))
    }

    fn lane<'a>(&'a self, entity: &'a str, group: &'a str) -> Lane<'a> {
        let label = self.labels.get(entity).map_or(entity, |l| l.as_str());
        Lane {
            entity,
            label,
            group,
            moments: vec![],
            depth: 0,
            children: 0,
        }
    }

    /// The entity's newest label, else its id.
    pub fn label_of<'a>(&'a self, entity: &'a str) -> &'a str {
        self.labels.get(entity).map_or(entity, String::as_str)
    }

    /// Entities whose parent is `entity`, in first-seen order.
    pub fn children_of(&self, entity: &str) -> Vec<&str> {
        let mut kids: Vec<&str> = self
            .parents
            .iter()
            .filter(|(_, p)| p.as_str() == entity)
            .map(|(c, _)| c.as_str())
            .filter(|c| *c != entity)
            .collect();
        kids.sort_by_key(|e| self.first_seen.get(*e).copied().unwrap_or(usize::MAX));
        kids
    }

    /// `entity` and everything nested under it.
    pub fn subtree(&self, entity: &str) -> HashSet<String> {
        let mut seen = HashSet::from([entity.to_string()]);
        let mut todo = vec![entity.to_string()];
        while let Some(e) = todo.pop() {
            for c in self.children_of(&e) {
                if seen.insert(c.to_string()) {
                    todo.push(c.to_string());
                }
            }
        }
        seen
    }

    pub fn parent_of(&self, entity: &str) -> Option<&str> {
        self.parents
            .get(entity)
            .map(String::as_str)
            .filter(|p| *p != entity)
    }

    /// Whether `entity` is among `of`'s ancestors (a loop in the data).
    fn is_ancestor(&self, entity: &str, of: &str) -> bool {
        let mut at = of;
        for _ in 0..32 {
            match self.parent_of(at) {
                Some(p) if p == entity => return true,
                Some(p) => at = p,
                None => return false,
            }
        }
        true
    }

    fn last_moments(&self, t: i64) -> HashMap<&str, i64> {
        let upto = self.moments.partition_point(|e| e.at <= t);
        self.moments[..upto]
            .iter()
            .map(|e| (e.entity.as_str(), e.at))
            .collect()
    }
}

fn to(e: &Event) -> &str {
    e.data["to"].as_str().unwrap_or("unknown")
}

pub fn field<'a>(e: &'a Event, key: &str) -> Option<&'a str> {
    e.data[key].as_str().filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(at: i64, entity: &str, kind: &str, to: Option<&str>) -> Event {
        Event {
            at,
            source: "s".into(),
            entity: entity.into(),
            kind: kind.into(),
            data: match to {
                Some(to) => {
                    json!({ "from": null, "to": to, "label": entity.to_uppercase(), "group": "g" })
                }
                None => json!({}),
            },
            r#ref: None,
        }
    }

    fn history(columns: &[&str], events: Vec<Event>) -> History {
        History::from_events(
            "S".into(),
            columns.iter().map(|c| c.to_string()).collect(),
            events,
        )
    }

    fn statuses(h: &History, t: i64) -> Vec<(String, Vec<String>)> {
        h.board_at(t)
            .into_iter()
            .filter(|c| !c.cards.is_empty())
            .map(|c| (c.status, c.cards.into_iter().map(|c| c.label).collect()))
            .collect()
    }

    #[test]
    fn board_follows_the_latest_state() {
        let h = history(
            &["new", "active", "done"],
            vec![
                ev(10, "a", "state", Some("new")),
                ev(20, "b", "state", Some("active")),
                ev(30, "a", "state", Some("done")),
            ],
        );
        let s = |v: &[(&str, &[&str])]| -> Vec<(String, Vec<String>)> {
            v.iter()
                .map(|(c, t)| (c.to_string(), t.iter().map(|x| x.to_string()).collect()))
                .collect()
        };
        assert_eq!(statuses(&h, 5), s(&[]));
        assert_eq!(statuses(&h, 25), s(&[("new", &["A"]), ("active", &["B"])]));
        assert_eq!(statuses(&h, 30), s(&[("active", &["B"]), ("done", &["A"])]));
    }

    #[test]
    fn columns_follow_the_description_then_first_appearance() {
        let h = history(
            &["new", "done"],
            vec![
                ev(1, "a", "state", Some("done")),
                ev(2, "b", "state", Some("archived")),
                ev(3, "c", "state", Some("new")),
            ],
        );
        let order: Vec<String> = h.board_at(3).into_iter().map(|c| c.status).collect();
        assert_eq!(order, ["new", "done", "archived"]);
        // Without a description, values keep the order they first appeared in.
        let h = history(
            &[],
            vec![
                ev(1, "a", "state", Some("done")),
                ev(2, "b", "state", Some("new")),
            ],
        );
        let order: Vec<String> = h.board_at(2).into_iter().map(|c| c.status).collect();
        assert_eq!(order, ["done", "new"]);
    }

    #[test]
    fn cards_glow_after_a_moment_and_fade() {
        let h = history(
            &[],
            vec![
                ev(0, "a", "state", Some("new")),
                ev(1000, "a", "command", None),
            ],
        );
        let glow = |t| h.board_at(t)[0].cards[0].glow;
        assert_eq!(glow(500), 0.0);
        assert_eq!(glow(1000), 1.0);
        assert!((glow(1000 + GLOW_MS / 2) - 0.5).abs() < 0.01);
        assert_eq!(glow(1000 + GLOW_MS), 0.0);
    }

    #[test]
    fn entity_detail_at_a_moment() {
        let h = history(
            &[],
            vec![
                ev(10, "a", "state", Some("new")),
                ev(20, "a", "note", None),
                ev(30, "a", "state", Some("done")),
            ],
        );
        assert_eq!(h.state_of("a", 25).map(to), Some("new"));
        let kinds: Vec<&str> = h
            .events_of("a", 25, 10)
            .iter()
            .map(|e| e.kind.as_str())
            .collect();
        assert_eq!(kinds, ["note", "state"]);
    }

    #[test]
    fn links_use_the_newest_values() {
        let dir =
            std::env::temp_dir().join(format!("ithildin-links-history-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("new-repo/done")).unwrap();
        std::fs::write(dir.join("new-repo/done/a.md"), "").unwrap();
        let mut h = history(
            &[],
            vec![
                Event {
                    data: serde_json::json!({ "to": "done", "group": "old-repo", "label": "a" }),
                    ..ev(1, "a", "state", Some("done"))
                },
                // The repo folder is renamed later; the state does not change, only a moment carries it.
                Event {
                    data: serde_json::json!({ "group": "new-repo", "label": "a" }),
                    ..ev(2, "a", "activity", None)
                },
            ],
        );
        h.links = sources::links(&serde_json::json!({ "links": [{ "label": "File", "url": format!("file://{}/{{group}}/{{state}}/{{label}}.md", dir.display()) }] })).unwrap();
        let got: Vec<String> = h.links_of("a").into_iter().map(|o| o.url).collect();
        assert_eq!(
            got,
            vec![format!("file://{}/new-repo/done/a.md", dir.display())]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn lanes_hold_the_moments_in_the_window_in_first_seen_order() {
        let h = history(
            &[],
            vec![
                ev(1, "b", "x", None),
                ev(2, "a", "x", None),
                ev(5, "a", "x", None),
                ev(6, "b", "x", None),
                ev(9, "c", "x", None),
            ],
        );
        let lanes = h.lanes(4, 7, &HashSet::new());
        let got: Vec<(&str, usize)> = lanes.iter().map(|l| (l.entity, l.moments.len())).collect();
        // a's moment comes first in the window, but b was seen first in the source.
        assert_eq!(got, [("b", 1), ("a", 1)]);
        assert!(h.lanes(10, 20, &HashSet::new()).is_empty());
    }

    #[test]
    fn child_lanes_nest_under_their_parent_when_it_is_expanded() {
        let child = |at, entity, parent: &str| Event {
            data: serde_json::json!({ "parent": parent }),
            ..ev(at, entity, "x", None)
        };
        let h = history(
            &[],
            vec![
                ev(1, "s", "x", None),
                child(2, "s/a", "s"),
                child(3, "s/a/b", "s/a"),
                ev(4, "t", "x", None),
                child(5, "s/c", "s"),
                child(6, "loop1", "loop2"),
                child(7, "loop2", "loop1"),
            ],
        );
        let shape = |expanded: &[&str]| -> Vec<(String, usize, usize)> {
            let expanded = expanded.iter().map(|s| s.to_string()).collect();
            // From 3: "s" itself has no moment there but still heads its children.
            h.lanes(3, 9, &expanded)
                .iter()
                .map(|l| (l.entity.to_string(), l.depth, l.children))
                .collect()
        };
        let lane = |e: &str, d, c| (e.to_string(), d, c);
        assert_eq!(
            shape(&[]),
            [
                lane("s", 0, 2),
                lane("t", 0, 0),
                lane("loop1", 0, 0),
                lane("loop2", 0, 0)
            ]
        );
        assert_eq!(
            shape(&["s", "s/a", "loop2"]),
            [
                lane("s", 0, 2),
                lane("s/a", 1, 1),
                lane("s/a/b", 2, 0),
                lane("s/c", 1, 0),
                lane("t", 0, 0),
                lane("loop1", 0, 0),
                lane("loop2", 0, 0)
            ]
        );
    }

    #[test]
    fn a_child_forks_off_the_parents_last_moment_before_it() {
        let child = |at, entity, parent: &str| Event {
            data: serde_json::json!({ "parent": parent }),
            ..ev(at, entity, "x", None)
        };
        let h = history(
            &[],
            vec![
                ev(1, "s", "x", None),
                ev(4, "s", "x", None),
                child(5, "s/a", "s"),
                ev(6, "s", "x", None),
                child(9, "s/a", "s"),
            ],
        );
        assert_eq!(h.fork("s/a"), Some((4, 5)));
        assert_eq!(h.fork("s"), None, "no parent");
    }

    #[test]
    fn a_range_shows_only_entities_with_events_in_it() {
        let mut h = history(
            &[],
            vec![
                ev(10, "old", "state", Some("done")), // finished before the range: hidden
                ev(15, "carried", "state", Some("active")), // went active before the range…
                ev(55, "carried", "activity", None),  // …and was worked on inside it
                ev(60, "new", "state", Some("new")),
            ],
        );
        h.set_range(Some((50, 100)));
        assert_eq!((h.start, h.end), (50, 100));
        assert_eq!(
            statuses(&h, 100),
            [
                ("active".to_string(), vec!["CARRIED".to_string()]),
                ("new".to_string(), vec!["NEW".to_string()])
            ]
        );
        assert_eq!(
            h.density(2),
            vec![2, 0],
            "only the two events inside the range"
        );
        h.set_range(None);
        assert_eq!((h.start, h.end), (10, 60));
        assert_eq!(statuses(&h, 100).len(), 3);
    }

    #[test]
    fn lanes_take_the_newest_label_and_links_see_every_event() {
        let mut h = history(
            &[],
            vec![
                Event {
                    data: serde_json::json!({ "subject": "first" }),
                    ..ev(1, "s", "prompt", None)
                },
                Event {
                    data: serde_json::json!({ "prUrl": "https://x/pr/7", "label": "Fix the board" }),
                    ..ev(2, "s", "pr", None)
                },
                Event {
                    data: serde_json::json!({ "subject": "second", "label": "Fix the board" }),
                    ..ev(3, "s", "prompt", None)
                },
            ],
        );
        // The window holds only the first prompt, which had no title yet: the lane still has it.
        assert_eq!(h.lanes(0, 1, &HashSet::new())[0].label, "Fix the board");
        h.links =
            sources::links(&serde_json::json!({ "links": [{ "label": "PR", "url": "{prUrl}" }] }))
                .unwrap();
        // The newest event has no prUrl; the PR event does.
        let got: Vec<(String, String)> = h
            .links_of("s")
            .into_iter()
            .map(|o| (o.label, o.url))
            .collect();
        assert_eq!(got, vec![("PR".to_string(), "https://x/pr/7".to_string())]);
    }

    #[test]
    fn groups_are_numbered_in_order_of_first_appearance() {
        let g = |at, entity, group: &str| Event {
            data: serde_json::json!({ "group": group }),
            ..ev(at, entity, "x", None)
        };
        let h = history(
            &[],
            vec![
                g(3, "c", "frontend"),
                g(1, "a", "backend"),
                g(2, "b", "backend"),
                g(4, "d", "testing"),
            ],
        );
        assert_eq!(
            (
                h.group_index("backend"),
                h.group_index("frontend"),
                h.group_index("testing")
            ),
            (0, 1, 2)
        );
        assert_eq!(h.group_index("never-seen"), 0);
    }

    #[test]
    fn density_and_next_event() {
        let h = history(
            &[],
            vec![
                ev(0, "a", "x", None),
                ev(1, "a", "x", None),
                ev(10, "a", "x", None),
            ],
        );
        assert_eq!(h.density(2), vec![2, 1]);
        assert_eq!(h.next_event(1), Some(10));
        assert_eq!(h.next_event(10), None);
    }
}

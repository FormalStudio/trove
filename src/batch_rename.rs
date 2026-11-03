//! The batch rename sheet: find and replace (plain or a regular expression) or numbering
//! ("Name 01"), a live preview of every new name with the ones that cannot be used flagged, and
//! one `Op::RenameMany` that Undo takes back whole.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use nd::prelude::*;
use trove_ops::Op;
use trove_ops::batch::{self, Item, Planned, Rule};

use crate::ops::{self, Origin};

const ROW_HEIGHT: f32 = 26.;
const MODES: [&str; 2] = ["Replace text", "Number"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Replace,
    Number,
}

pub struct BatchRename {
    origin: Origin,
    paths: Vec<PathBuf>,
    items: Vec<Item>,
    siblings: HashMap<PathBuf, HashSet<String>>,
    loaded: bool,
    mode: Mode,
    find: SharedString,
    with: SharedString,
    regex: bool,
    base: SharedString,
    start: u64,
    digits: usize,
    plan: Vec<Planned>,
    /// Why the pattern does not compile.
    error: Option<String>,
    list: ListState<u32>,
    versions: Vec<u64>,
    generation: u64,
}

/// Opens the sheet for `paths`, in the order the listing shows them, over the origin's window.
pub fn open(cx: &mut App, origin: Origin, paths: Vec<PathBuf>) -> WindowHandle<BatchRename> {
    let options = WindowOptions::main("main.rename")
        .title(format!("Rename {} items", paths.len()))
        .kind(WindowKind::Sheet { parent: origin.window })
        .size(size(px(560.), px(540.)));
    cx.open_window(options, move |_, cx| cx.new(|cx| BatchRename::new(origin, paths, cx)))
}

/// The sheet over a demo window, for the appearance audit: three chapters renumbered, one of
/// them onto a name that is taken.
pub fn open_demo(cx: &mut App) -> nd::core::WindowId {
    let parent = crate::open_demo(cx);
    let docs = crate::demo::root().join("Documents");
    let paths = ["chapter1.md", "chapter2.md", "chapter10.md"].map(|n| docs.join(n)).to_vec();
    let sheet = open(cx, Origin { window: parent.id(), tab: None }, paths);
    ops::run_deferred(cx);
    sheet.root().clone().update(cx, |s, cx| {
        s.set_find("chapter".into(), cx);
        s.set_with("Chapter ".into(), cx);
    });
    sheet.id()
}

impl BatchRename {
    fn new(origin: Origin, paths: Vec<PathBuf>, cx: &mut Context<Self>) -> Self {
        let base = paths
            .first()
            .and_then(|p| p.parent())
            .map(ops::name_of)
            .filter(|n| !n.is_empty() && n != "/")
            .unwrap_or_else(|| "Item".to_owned());
        let read = paths.clone();
        ops::background(
            cx.weak_entity(),
            move || {
                let items: Vec<Item> = read
                    .into_iter()
                    .map(|path| {
                        let is_dir = std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir());
                        Item { path, is_dir }
                    })
                    .collect();
                let siblings = batch::siblings(&items);
                (items, siblings)
            },
            |this: &mut Self, (items, siblings), cx| {
                this.items = items;
                this.siblings = siblings;
                this.loaded = true;
                this.replan(cx);
            },
        );
        Self {
            origin,
            paths,
            items: Vec::new(),
            siblings: HashMap::new(),
            loaded: false,
            mode: Mode::Replace,
            find: SharedString::default(),
            with: SharedString::default(),
            regex: false,
            base: base.into(),
            start: 1,
            digits: 2,
            plan: Vec::new(),
            error: None,
            list: ListState::new(cx).estimated_row_height(px(ROW_HEIGHT)),
            versions: Vec::new(),
            generation: 0,
        }
    }

    fn rule(&self) -> Rule {
        match self.mode {
            Mode::Replace => {
                Rule::Replace { find: self.find.to_string(), with: self.with.to_string(), regex: self.regex }
            }
            Mode::Number => Rule::Number { base: self.base.to_string(), start: self.start, digits: self.digits },
        }
    }

    /// The preview again after a change to the rule; only rows whose new name changed redraw.
    fn replan(&mut self, cx: &mut Context<Self>) {
        let plan = match batch::plan(&self.items, &self.rule(), &self.siblings) {
            Ok(plan) => {
                self.error = None;
                plan
            }
            Err(why) => {
                self.error = Some(why);
                self.items
                    .iter()
                    .map(|i| Planned { path: i.path.clone(), name: ops::name_of(&i.path), problem: None })
                    .collect()
            }
        };
        self.generation += 1;
        self.versions.resize(plan.len(), 0);
        for (i, row) in plan.iter().enumerate() {
            if self.plan.get(i) != Some(row) {
                self.versions[i] = self.generation;
            }
        }
        self.plan = plan;
        let keys = self.versions.iter().enumerate().map(|(i, v)| (i as u32, *v));
        self.list.set_items_versioned(keys, cx);
        cx.notify();
    }

    pub fn plan(&self) -> &[Planned] {
        &self.plan
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// What the Rename button would submit: `None` while any name has a problem or none changes.
    pub fn renames(&self) -> Option<Vec<(PathBuf, String)>> {
        if self.error.is_some() || !self.loaded {
            return None;
        }
        batch::renames(&self.plan)
    }

    pub fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        self.mode = mode;
        self.replan(cx);
    }

    pub fn set_find(&mut self, text: SharedString, cx: &mut Context<Self>) {
        self.find = text;
        self.replan(cx);
    }

    pub fn set_with(&mut self, text: SharedString, cx: &mut Context<Self>) {
        self.with = text;
        self.replan(cx);
    }

    pub fn set_regex(&mut self, on: bool, cx: &mut Context<Self>) {
        self.regex = on;
        self.replan(cx);
    }

    pub fn set_base(&mut self, text: SharedString, cx: &mut Context<Self>) {
        self.base = text;
        self.replan(cx);
    }

    pub fn set_start(&mut self, start: u64, cx: &mut Context<Self>) {
        self.start = start;
        self.replan(cx);
    }

    pub fn set_digits(&mut self, digits: usize, cx: &mut Context<Self>) {
        self.digits = digits;
        self.replan(cx);
    }

    /// Runs the renames as one job and closes the sheet.
    pub fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(renames) = self.renames() else { return };
        ops::submit(cx, self.origin.clone(), Op::RenameMany { renames });
        cx.close_window(window.id());
    }

    /// "2 of 3 names change", or what stops the rename.
    fn summary(&self) -> (String, bool) {
        if let Some(why) = &self.error {
            return (why.clone(), true);
        }
        let problems = self.plan.iter().filter(|p| p.problem.is_some()).count();
        if problems > 0 {
            let names = if problems == 1 { "1 name".to_owned() } else { format!("{problems} names") };
            return (format!("{names} can\u{2019}t be used"), true);
        }
        let changed = self.plan.iter().filter(|p| p.changed()).count();
        (format!("{changed} of {} names change", self.paths.len()), false)
    }

    fn row(&mut self, key: u32, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let Some(p) = self.plan.get(key as usize) else { return div().into_any_element() };
        let old = ops::name_of(&p.path);
        let new_color = match (p.problem, p.changed()) {
            (Some(_), _) => theme.text.danger,
            (None, true) => theme.text.primary,
            (None, false) => theme.text.tertiary,
        };
        h_flex()
            .id(format!("rename-row-{key}"))
            .h(px(ROW_HEIGHT))
            .px(theme.spacing.sm)
            .gap(theme.spacing.sm)
            .items_center()
            .child(text(old).text_color(theme.text.secondary).truncate().flex_1().min_w_0())
            .child(icon(Icon::ChevronRight, 12.).text_color(theme.text.tertiary).flex_none())
            .child(text(p.name.clone()).text_color(new_color).truncate().flex_1().min_w_0())
            .children(p.problem.map(|why| {
                text(why.message()).text_size(theme.type_scale.footnote).text_color(theme.text.danger).flex_none()
            }))
            .into_any_element()
    }
}

impl Render for BatchRename {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let labelled = |label: &'static str, control: AnyElement| {
            h_flex()
                .w_full()
                .gap(theme.spacing.md)
                .items_center()
                .child(text(label).text_color(theme.text.secondary).flex_none().w(px(96.)).text_right())
                .child(div().flex_1().min_w_0().child(control))
        };
        let mode = {
            let me = cx.weak_entity();
            segmented("rename-mode", MODES).selected(if self.mode == Mode::Replace { 0 } else { 1 }).on_change(
                move |e: &SelectChanged, _, cx| {
                    let mode = if e.index == 0 { Mode::Replace } else { Mode::Number };
                    let _ = me.update(cx, |this, cx| this.set_mode(mode, cx));
                },
            )
        };
        let field = |id: &'static str,
                     label: &'static str,
                     value: SharedString,
                     set: fn(&mut Self, SharedString, &mut Context<Self>),
                     cx: &Context<Self>| {
            let me = cx.weak_entity();
            text_field(id).label(label).text(value).w_full().on_change(move |e: &TextChanged, _, cx| {
                let _ = me.update(cx, |this, cx| set(this, e.text.clone(), cx));
            })
        };
        let controls = match self.mode {
            Mode::Replace => {
                let me = cx.weak_entity();
                v_flex()
                    .gap(theme.spacing.sm)
                    .child(labelled(
                        "Find",
                        field("rename-find", "Find", self.find.clone(), Self::set_find, cx).into_any_element(),
                    ))
                    .child(labelled(
                        "Replace with",
                        field("rename-with", "Replace with", self.with.clone(), Self::set_with, cx).into_any_element(),
                    ))
                    .child(labelled(
                        "",
                        checkbox("rename-regex", "Regular expression")
                            .checked(self.regex)
                            .on_toggle(move |e: &Toggled, _, cx| {
                                let _ = me.update(cx, |this, cx| this.set_regex(e.checked, cx));
                            })
                            .into_any_element(),
                    ))
            }
            Mode::Number => {
                let (start, digits) = (cx.weak_entity(), cx.weak_entity());
                v_flex()
                    .gap(theme.spacing.sm)
                    .child(labelled(
                        "Name",
                        field("rename-base", "Name", self.base.clone(), Self::set_base, cx).into_any_element(),
                    ))
                    .child(labelled(
                        "Start at",
                        stepper("rename-start", self.start as f64)
                            .range(0.0..=99_999.0)
                            .with_field(0)
                            .label("Start at")
                            .on_change(move |e: &StepperChanged, _, cx| {
                                let _ = start.update(cx, |this, cx| this.set_start(e.value.max(0.) as u64, cx));
                            })
                            .into_any_element(),
                    ))
                    .child(labelled(
                        "Digits",
                        stepper("rename-digits", self.digits as f64)
                            .range(1.0..=6.0)
                            .with_field(0)
                            .label("Digits")
                            .on_change(move |e: &StepperChanged, _, cx| {
                                let _ =
                                    digits.update(cx, |this, cx| this.set_digits(e.value.clamp(1., 6.) as usize, cx));
                            })
                            .into_any_element(),
                    ))
            }
        };
        let (summary, bad) = self.summary();
        let can_rename = self.renames().is_some();
        let commit = cx.weak_entity();
        let title = format!("Rename {} items", self.paths.len());
        div()
            .id("rename-sheet")
            .size_full()
            .on_action(|_: &nd::actions::Cancel, window, cx| cx.close_window(window.id()))
            .child(
                sheet_layout(title)
                    .child(
                        v_flex()
                            .size_full()
                            .gap(theme.spacing.md)
                            .child(div().child(mode))
                            .child(controls)
                            .child(
                                text(summary)
                                    .id("rename-summary")
                                    .text_size(theme.type_scale.footnote)
                                    .text_color(if bad { theme.text.danger } else { theme.text.secondary }),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_h_0()
                                    .rounded(theme.radius.md)
                                    .border_1()
                                    .border_color(theme.border.subtle)
                                    .child(list("rename-preview", &self.list, cx.processor(Self::row))),
                            ),
                    )
                    .footer(
                        button("rename-cancel", "Cancel")
                            .on_click(|_: &ClickEvent, window, cx| cx.close_window(window.id())),
                    )
                    .footer(button("rename-commit", "Rename").primary().disabled(!can_rename).on_click(
                        move |_: &ClickEvent, window, cx| {
                            let _ = commit.update(cx, |this, cx| this.commit(window, cx));
                        },
                    )),
            )
    }
}

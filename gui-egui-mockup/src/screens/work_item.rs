//! Work-item detail: the markdown body, comments, the latest hand-off, relations, branches and
//! linked sessions.

use fluent2_egui::IconName;
use fluent2_egui::controls::{
    Appearance, Avatar, BadgeColor, Breadcrumb, Button, Link, MenuButton, Textarea,
};
use fluent2_egui::data::markdown::{Document, MarkdownView};
use fluent2_egui::surfaces::{Intent, MenuItem, Toast, Toaster};

use super::{Action, caption, facts, outline, priority, tint, work_state};
use crate::app::Screen;
use crate::fixtures::{self, Comment, Fixtures, WorkState};

/// Work-item screen state.
pub struct WorkItemScreen {
    /// The reference shown.
    pub reference: String,
    /// Draft comment.
    pub draft: String,
    body: Document,
    comments: Vec<(Comment, Document)>,
}

impl Default for WorkItemScreen {
    fn default() -> Self {
        Self {
            reference: "WI-2162".to_owned(),
            draft: String::new(),
            body: Document::parse(fixtures::WORK_BODY),
            comments: fixtures::comments()
                .into_iter()
                .map(|comment| {
                    let doc = Document::parse(&comment.body);
                    (comment, doc)
                })
                .collect(),
        }
    }
}

impl WorkItemScreen {
    /// Draw the screen.
    pub fn show(&mut self, ui: &mut egui::Ui, data: &mut Fixtures) -> Option<Action> {
        let mut action = None;
        let Some(item) = data.work_item(&self.reference).cloned() else {
            ui.label(format!("No work item {}.", self.reference));
            return None;
        };
        let crumbs =
            Breadcrumb::new("work_crumbs", ["Backlog", item.project, item.reference]).show(ui);
        if crumbs.clicked == Some(0) {
            action = Some(Action::Go(Screen::Backlog));
        }
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new(item.reference)
                    .size(22.0)
                    .monospace()
                    .weak(),
            );
            ui.label(egui::RichText::new(item.title).size(22.0).strong());
        });
        ui.horizontal_wrapped(|ui| {
            work_state(ui, item.state);
            priority(ui, item.priority);
            outline(ui, item.kind, BadgeColor::Informative);
            outline(ui, item.project, BadgeColor::Subtle);
            caption(
                ui,
                format!("score {:.1} · updated {}", item.score, item.updated),
            );
            ui.add_space(16.0);
            ui.add(
                Button::new("Start session")
                    .icon(IconName::Window)
                    .appearance(Appearance::Primary),
            );
            ui.add(
                Button::new("Bind branch")
                    .icon(IconName::Link)
                    .appearance(Appearance::Subtle),
            );
            MenuButton::new("Transition").show(ui, |ui| {
                for state in WorkState::ALL {
                    if ui.add(MenuItem::new(state.label())).clicked()
                        && let Some(target) =
                            data.work.iter_mut().find(|w| w.reference == item.reference)
                    {
                        target.state = state;
                    }
                }
            });
        });
        ui.add_space(8.0);
        egui::Panel::right("work_side")
            .resizable(true)
            .default_size(320.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("work_side_scroll")
                    .show(ui, |ui| {
                        if let Some(a) = side(ui, data) {
                            action = Some(a);
                        }
                    });
            });
        egui::ScrollArea::vertical()
            .id_salt("work_main")
            .show(ui, |ui| {
                ui.set_width(ui.available_width() - 12.0);
                let _ = MarkdownView::new("work_body", &self.body).show(ui);
                ui.add_space(12.0);
                handoff(ui);
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new(format!("Comments ({})", self.comments.len()))
                        .size(18.0)
                        .strong(),
                );
                ui.add_space(6.0);
                for (index, (comment, doc)) in self.comments.iter().enumerate() {
                    ui.horizontal_top(|ui| {
                        ui.add(Avatar::new(comment.author).size(32.0));
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.strong(comment.author);
                                let color = match comment.kind {
                                    "decision" => BadgeColor::Important,
                                    "handoff" => BadgeColor::Success,
                                    _ => BadgeColor::Subtle,
                                };
                                outline(ui, comment.kind, color);
                                caption(ui, comment.when);
                            });
                            let _ = MarkdownView::new(("comment", index), doc).show(ui);
                        });
                    });
                    ui.add_space(10.0);
                }
                ui.add(
                    Textarea::new(&mut self.draft)
                        .label("Add a comment")
                        .rows(3),
                );
                ui.horizontal(|ui| {
                    if ui
                        .add(Button::new("Comment").appearance(Appearance::Primary))
                        .clicked()
                        && !self.draft.trim().is_empty()
                    {
                        let body = std::mem::take(&mut self.draft);
                        let doc = Document::parse(&body);
                        self.comments.push((
                            Comment {
                                author: "sprooty",
                                kind: "note",
                                when: "just now",
                                body,
                            },
                            doc,
                        ));
                        Toaster::push(
                            ui.ctx(),
                            Toast {
                                intent: Intent::Info,
                                title: "Comment added locally".to_owned(),
                                body: Some("Mockup: nothing was sent.".to_owned()),
                                timeout: None,
                                action: None,
                            },
                        );
                    }
                    caption(ui, "Markdown is supported.");
                });
            });
        action
    }
}

/// The hand-off fields of the latest hand-off comment.
fn handoff(ui: &mut egui::Ui) {
    super::card(ui, |ui| {
        ui.horizontal(|ui| {
            tint(ui, "Hand-off", BadgeColor::Success);
            ui.strong("wi-2162-fluent-mockup");
            caption(ui, "draft · not posted yet");
        });
        ui.add_space(6.0);
        facts(
            ui,
            "handoff_facts",
            &[
                ("Branch", "wi-2162-vogt-fluent-mockup".to_owned()),
                ("Head", "3f9a1c2".to_owned()),
                ("Base", "main @ 1938173".to_owned()),
                (
                    "Done",
                    "Shell, palette, shortcuts, 9 screens, both themes".to_owned(),
                ),
                ("Remaining", "Screenshots in CI".to_owned()),
                (
                    "Gates run",
                    "clippy -D warnings · wasm build · kittest (4)".to_owned(),
                ),
                ("Blockers", "None".to_owned()),
                ("Verified", "native + wasm, local trunk serve".to_owned()),
            ],
        );
    });
}

fn side(ui: &mut egui::Ui, data: &Fixtures) -> Option<Action> {
    let mut action = None;
    super::card(ui, |ui| {
        ui.strong("Relations");
        ui.add_space(4.0);
        for (relation, reference) in [
            ("Parent", "WI-1806"),
            ("Related", "WI-2003"),
            ("Blocks", "WI-2147"),
            ("Depends on", "WI-2149"),
        ] {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(relation).weak());
                let title = data
                    .work_item(reference)
                    .map_or("Vogt GUI on egui", |w| w.title);
                if ui.add(Link::new(format!("{reference} {title}"))).clicked()
                    && data.work_item(reference).is_some()
                {
                    action = Some(Action::OpenWork(reference.to_owned()));
                }
            });
        }
    });
    ui.add_space(8.0);
    super::card(ui, |ui| {
        ui.strong("Branches");
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("wi-2162-vogt-fluent-mockup").monospace());
        });
        ui.horizontal(|ui| {
            tint(ui, "pushed", BadgeColor::Success);
            outline(ui, "no PR", BadgeColor::Subtle);
            outline(ui, "CI not run", BadgeColor::Subtle);
        });
        caption(ui, "+2,140 −0 · 14 files · 6 commits ahead of main");
        if ui
            .add(Button::new("View diff").appearance(Appearance::Subtle))
            .clicked()
        {
            action = Some(Action::Go(Screen::Git));
        }
    });
    ui.add_space(8.0);
    super::card(ui, |ui| {
        ui.strong("Sessions");
        ui.add_space(4.0);
        for session in data
            .sessions
            .iter()
            .filter(|s| s.work == Some("WI-2162") || s.name == "oversight")
        {
            ui.horizontal(|ui| {
                super::session_state(ui, session.state);
                if ui.add(Link::new(session.name)).clicked() {
                    action = Some(Action::OpenSession(session.id.to_owned()));
                }
            });
        }
    });
    ui.add_space(8.0);
    super::card(ui, |ui| {
        ui.strong("Details");
        ui.add_space(4.0);
        facts(
            ui,
            "work_detail_facts",
            &[
                ("Created", "2026-10-22 by sprooty".to_owned()),
                ("Trust", "unverified".to_owned()),
                ("Lands by", "push branch".to_owned()),
                ("Merge by", "oversight (rebase)".to_owned()),
                ("Deploy by", "operator".to_owned()),
            ],
        );
    });
    action
}

//! Backlog: a filter bar (search, project, priority, state, kind) over a sortable DataGrid.
//! Enter or double-click on a row opens the work item.

use fluent2_egui::IconName;
use fluent2_egui::controls::{Appearance, Button, Dropdown, SearchBox};
use fluent2_egui::data::data_grid::{
    Align, Column, ColumnWidth, DataGrid, DataGridSize, DataGridState, SortDirection, SortSpec,
};
use fluent2_egui::data::{RowId, SelectionMode};

use super::{Action, caption, heading, priority, work_state};
use crate::fixtures::{Fixtures, WorkItem, WorkState};

const PROJECTS: [&str; 6] = [
    "Any project",
    "vogt",
    "fluent2-egui",
    "cadastre",
    "farmeggs",
    "ops",
];
const PRIORITIES: [&str; 5] = ["Any priority", "p0", "p1", "p2", "p3"];
const STATES: [&str; 6] = [
    "Any state",
    "Open",
    "In progress",
    "Review",
    "Blocked",
    "Done",
];
const KINDS: [&str; 6] = ["Any kind", "feature", "bug", "chore", "design", "docs"];

/// Backlog screen state.
pub struct BacklogScreen {
    /// Search text.
    pub query: String,
    /// Project filter; 0 is any.
    pub project: Option<usize>,
    /// Priority filter; 0 is any.
    pub priority: Option<usize>,
    /// State filter; 0 is any.
    pub state: Option<usize>,
    /// Kind filter; 0 is any.
    pub kind: Option<usize>,
    /// Hide done items.
    pub hide_done: bool,
    /// Grid state: sort, widths, selection.
    pub grid: DataGridState,
}

impl Default for BacklogScreen {
    fn default() -> Self {
        Self {
            query: String::new(),
            project: Some(0),
            priority: Some(0),
            state: Some(0),
            kind: Some(0),
            hide_done: true,
            grid: DataGridState {
                sort: Some(SortSpec {
                    column: "score".to_owned(),
                    direction: SortDirection::Descending,
                }),
                ..DataGridState::default()
            },
        }
    }
}

impl BacklogScreen {
    /// The rows that pass the filters, in the grid's sort order.
    pub fn rows<'a>(&self, data: &'a Fixtures) -> Vec<&'a WorkItem> {
        let pick = |choice: Option<usize>, names: &[&'static str]| -> Option<&'static str> {
            choice
                .filter(|i| *i > 0)
                .and_then(|i| names.get(i).copied())
        };
        let project = pick(self.project, &PROJECTS);
        let prio = pick(self.priority, &PRIORITIES);
        let state = pick(self.state, &STATES);
        let kind = pick(self.kind, &KINDS);
        let query = self.query.to_lowercase();
        let mut rows: Vec<&WorkItem> = data
            .work
            .iter()
            .filter(|item| project.is_none_or(|p| item.project == p))
            .filter(|item| prio.is_none_or(|p| item.priority == p))
            .filter(|item| state.is_none_or(|s| item.state.label() == s))
            .filter(|item| kind.is_none_or(|k| item.kind == k))
            .filter(|item| !(self.hide_done && state.is_none() && item.state == WorkState::Done))
            .filter(|item| {
                query.is_empty()
                    || item.title.to_lowercase().contains(&query)
                    || item.reference.to_lowercase().contains(&query)
            })
            .collect();
        if let Some(sort) = &self.grid.sort {
            rows.sort_by(|a, b| {
                let order = match sort.column.as_str() {
                    "ref" => a.reference.cmp(b.reference),
                    "title" => a.title.cmp(b.title),
                    "project" => a.project.cmp(b.project),
                    "kind" => a.kind.cmp(b.kind),
                    "priority" => a.priority.cmp(b.priority),
                    "state" => a.state.cmp(&b.state),
                    "updated" => a.age_days.total_cmp(&b.age_days),
                    _ => a.score.total_cmp(&b.score),
                };
                match sort.direction {
                    SortDirection::Ascending => order,
                    SortDirection::Descending => order.reverse(),
                }
            });
        }
        rows
    }

    /// Draw the screen.
    pub fn show(&mut self, ui: &mut egui::Ui, data: &mut Fixtures) -> Option<Action> {
        heading(
            ui,
            "Backlog",
            "Every open work item, ranked by score. Enter or double-click opens one.",
        );
        ui.horizontal_wrapped(|ui| {
            ui.add(
                SearchBox::new(&mut self.query)
                    .placeholder("Search titles and refs")
                    .label("Search the backlog")
                    .desired_width(240.0),
            );
            ui.add(Dropdown::new("filter_project", &mut self.project, &PROJECTS).width(150.0));
            ui.add(Dropdown::new("filter_priority", &mut self.priority, &PRIORITIES).width(130.0));
            ui.add(Dropdown::new("filter_state", &mut self.state, &STATES).width(130.0));
            ui.add(Dropdown::new("filter_kind", &mut self.kind, &KINDS).width(120.0));
            ui.add(fluent2_egui::controls::Switch::new(
                &mut self.hide_done,
                "Hide done",
            ));
            if ui
                .add(
                    Button::new("Clear")
                        .icon(IconName::Dismiss)
                        .appearance(Appearance::Subtle),
                )
                .clicked()
            {
                let sort = self.grid.sort.clone();
                *self = Self::default();
                self.grid.sort = sort;
            }
        });
        ui.add_space(6.0);
        let rows = self.rows(data);
        caption(
            ui,
            format!("{} of {} work items", rows.len(), data.work.len()),
        );
        ui.add_space(4.0);
        let columns = [
            Column::new("ref", "Ref").width(ColumnWidth::Fixed(90.0)),
            Column::new("title", "Title").min_width(220.0),
            Column::new("project", "Project").width(ColumnWidth::Fixed(110.0)),
            Column::new("kind", "Kind").width(ColumnWidth::Fixed(84.0)),
            Column::new("priority", "Priority").width(ColumnWidth::Fixed(80.0)),
            Column::new("state", "State").width(ColumnWidth::Fixed(110.0)),
            Column::new("score", "Score")
                .width(ColumnWidth::Fixed(72.0))
                .align(Align::End),
            Column::new("updated", "Updated").width(ColumnWidth::Fixed(96.0)),
        ];
        let text = |row: usize, column: &str| -> String {
            let Some(item) = rows.get(row) else {
                return String::new();
            };
            match column {
                "ref" => item.reference.to_owned(),
                "title" => item.title.to_owned(),
                "project" => item.project.to_owned(),
                "kind" => item.kind.to_owned(),
                "priority" => item.priority.to_owned(),
                "state" => item.state.label().to_owned(),
                "score" => format!("{:.1}", item.score),
                _ => item.updated.to_owned(),
            }
        };
        let response = DataGrid::new("backlog_grid", &columns, rows.len())
            .label("Backlog")
            .size(DataGridSize::Small)
            .selection_mode(SelectionMode::Single)
            .row_id(|row| RowId(row as u64))
            .cell_text(text)
            .show(ui, &mut self.grid, |cell, ui| {
                let Some(item) = rows.get(cell.row) else {
                    return;
                };
                match cell.column {
                    "priority" => {
                        priority(ui, item.priority);
                    }
                    "state" => {
                        work_state(ui, item.state);
                    }
                    "ref" => {
                        ui.label(egui::RichText::new(item.reference).monospace());
                    }
                    column => {
                        ui.add(egui::Label::new(text(cell.row, column)).truncate());
                    }
                }
            });
        response
            .activated
            .and_then(|cell| rows.get(cell.row))
            .map(|item| Action::OpenWork(item.reference.to_owned()))
    }
}

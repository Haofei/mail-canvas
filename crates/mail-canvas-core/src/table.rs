use anyhow::{Result, bail};
use kuchiki::NodeRef;

use crate::dom::{attr, element_tag};
use crate::{Length, parse_length};

#[derive(Debug)]
pub(crate) struct TableGrid {
    pub(crate) rows: Vec<TableRow>,
    pub(crate) column_count: usize,
    pub(crate) col_widths: Vec<Option<Length>>,
}

#[derive(Debug)]
pub(crate) struct TableRow {
    pub(crate) node: NodeRef,
    pub(crate) cells: Vec<TableCell>,
}

#[derive(Debug)]
pub(crate) struct TableCell {
    pub(crate) node: NodeRef,
    pub(crate) col: usize,
    pub(crate) colspan: usize,
}

pub(crate) fn build_table_grid(table: &NodeRef, max_table_cells: usize) -> Result<TableGrid> {
    let rows = collect_rows(table);
    let mut active_rowspans: Vec<usize> = Vec::new();
    let mut grid_rows = Vec::with_capacity(rows.len());
    let mut column_count = 0usize;
    let mut occupied_slots = 0usize;

    for row in rows {
        let mut col = 0usize;
        let mut cells = Vec::new();

        for cell in collect_cells(&row) {
            while active_rowspans.get(col).copied().unwrap_or(0) > 0 {
                col += 1;
            }

            let colspan = parse_span_attr(&cell, "colspan");
            let rowspan = parse_span_attr(&cell, "rowspan");
            occupied_slots = occupied_slots.saturating_add(colspan.saturating_mul(rowspan));
            if occupied_slots > max_table_cells {
                bail!(
                    "table cell slots exceed max-table-cells: {occupied_slots} > {max_table_cells}"
                );
            }
            if active_rowspans.len() < col + colspan {
                active_rowspans.resize(col + colspan, 0);
            }

            cells.push(TableCell {
                node: cell,
                col,
                colspan,
            });

            for occupied in &mut active_rowspans[col..col + colspan] {
                *occupied = (*occupied).max(rowspan);
            }
            col += colspan;
        }

        for occupied in &mut active_rowspans {
            *occupied = occupied.saturating_sub(1);
        }

        column_count = column_count.max(col).max(active_rowspans.len());
        grid_rows.push(TableRow { node: row, cells });
    }

    let mut col_widths = collect_col_widths(table);
    if col_widths.len() <= 1 && !grid_rows.iter().any(|row| row.cells.len() > 1) {
        column_count = column_count.min(1);
        for row in &mut grid_rows {
            for cell in &mut row.cells {
                cell.col = 0;
                cell.colspan = 1;
            }
        }
    }
    if col_widths.len() < column_count {
        col_widths.resize(column_count, None);
    }

    Ok(TableGrid {
        rows: grid_rows,
        column_count,
        col_widths,
    })
}

pub(crate) fn length_is_intrinsic_fixed(length: &Length) -> bool {
    matches!(length, Length::Px(_) | Length::Inherit)
}

pub(crate) fn column_offset(widths: &[f32], col: usize, spacing: f32) -> f32 {
    widths.iter().take(col).copied().sum::<f32>() + spacing * col as f32
}

pub(crate) fn spanned_width(widths: &[f32], col: usize, colspan: usize, spacing: f32) -> f32 {
    let end = (col + colspan).min(widths.len());
    let span = end.saturating_sub(col).max(1);
    widths[col.min(widths.len().saturating_sub(1))..end]
        .iter()
        .copied()
        .sum::<f32>()
        + spacing * span.saturating_sub(1) as f32
}

pub(crate) fn distribute_fixed_table_column_widths(
    widths: Vec<Option<f32>>,
    available: f32,
) -> Vec<f32> {
    let count = widths.len().max(1);
    let fixed_total: f32 = widths.iter().flatten().sum();
    let auto_count = widths.iter().filter(|width| width.is_none()).count();

    if auto_count > 0 {
        let auto_width = ((available - fixed_total).max(auto_count as f32)) / auto_count as f32;
        return widths
            .into_iter()
            .map(|width| width.unwrap_or(auto_width).max(1.0))
            .collect();
    }

    if fixed_total > 0.0 {
        let scale = available / fixed_total;
        return widths
            .into_iter()
            .map(|width| width.unwrap_or(0.0) * scale)
            .map(|width| width.max(1.0))
            .collect();
    }

    vec![(available / count as f32).max(1.0); count]
}

fn collect_rows(node: &NodeRef) -> Vec<NodeRef> {
    let mut rows = Vec::new();
    collect_rows_inner(node, &mut rows);
    rows
}

fn collect_rows_inner(node: &NodeRef, rows: &mut Vec<NodeRef>) {
    for child in node.children() {
        match element_tag(&child).as_deref() {
            Some("tr") => rows.push(child),
            Some("thead" | "tbody" | "tfoot") => collect_rows_inner(&child, rows),
            _ => {}
        }
    }
}

fn collect_cells(row: &NodeRef) -> Vec<NodeRef> {
    row.children()
        .filter(|child| matches!(element_tag(child).as_deref(), Some("td" | "th")))
        .collect()
}

fn collect_col_widths(table: &NodeRef) -> Vec<Option<Length>> {
    let mut widths = Vec::new();
    collect_col_widths_inner(table, &mut widths);
    widths
}

fn collect_col_widths_inner(node: &NodeRef, widths: &mut Vec<Option<Length>>) {
    for child in node.children() {
        match element_tag(&child).as_deref() {
            Some("col") => {
                let width = attr(&child, "width").and_then(|value| parse_length(&value));
                let span = parse_span_attr(&child, "span");
                widths.extend(std::iter::repeat_n(width, span));
            }
            Some("colgroup") => {
                let before = widths.len();
                collect_col_widths_inner(&child, widths);
                if widths.len() == before {
                    let width = attr(&child, "width").and_then(|value| parse_length(&value));
                    let span = parse_span_attr(&child, "span");
                    widths.extend(std::iter::repeat_n(width, span));
                }
            }
            _ => {}
        }
    }
}

fn parse_span_attr(node: &NodeRef, attr_name: &str) -> usize {
    attr(node, attr_name)
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(1)
        .min(32)
}

#[cfg(test)]
mod tests {
    use kuchiki::traits::TendrilSink as _;

    use super::*;
    use crate::dom::find_first_tag;

    #[test]
    fn col_span_expands_column_widths() {
        let document = kuchiki::parse_html()
            .one(r#"<table><col width="120" span="2"><tr><td>A</td><td>B</td></tr></table>"#);
        let table = find_first_tag(&document, "table").expect("table");
        let grid = build_table_grid(&table, 100).expect("grid");
        assert_eq!(grid.column_count, 2);
        assert_eq!(grid.col_widths.len(), 2);
        assert!(matches!(grid.col_widths[0], Some(Length::Px(120.0))));
        assert!(matches!(grid.col_widths[1], Some(Length::Px(120.0))));
    }

    #[test]
    fn empty_colgroup_width_expands_columns() {
        let document = kuchiki::parse_html().one(
            r#"<table><colgroup width="50%" span="3"></colgroup><tr><td>A</td><td>B</td><td>C</td></tr></table>"#,
        );
        let table = find_first_tag(&document, "table").expect("table");
        let grid = build_table_grid(&table, 100).expect("grid");
        assert_eq!(grid.column_count, 3);
        assert_eq!(grid.col_widths.len(), 3);
        assert!(
            matches!(grid.col_widths[0], Some(Length::Percent(value)) if (value - 0.5).abs() < f32::EPSILON)
        );
        assert!(
            matches!(grid.col_widths[1], Some(Length::Percent(value)) if (value - 0.5).abs() < f32::EPSILON)
        );
        assert!(
            matches!(grid.col_widths[2], Some(Length::Percent(value)) if (value - 0.5).abs() < f32::EPSILON)
        );
    }

    #[test]
    fn single_cell_colspan_spacers_do_not_create_narrow_footer_columns() {
        let document = kuchiki::parse_html().one(
            r#"<table width="91%">
                <tr><td colspan="2" height="35">&nbsp;</td></tr>
                <tr><td>Footer disclosure should use the table width.</td></tr>
              </table>"#,
        );
        let table = find_first_tag(&document, "table").expect("table");
        let grid = build_table_grid(&table, 100).expect("grid");

        assert_eq!(grid.column_count, 1);
        assert_eq!(grid.rows[0].cells[0].colspan, 1);
        assert_eq!(grid.rows[1].cells[0].colspan, 1);
    }
}

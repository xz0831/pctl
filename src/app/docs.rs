//! The documents the sidebar lists under their project, and the notice when one arrives.
use serde_json::Value;

/// One document of one computer, as the `list` answer gives it.
#[derive(Clone, Debug, PartialEq)]
pub struct DocRow {
    /// "" is this computer.
    pub machine: String,
    pub id: String,
    pub project: String,
    pub title: String,
    /// The session that showed it.
    pub by: Option<String>,
    pub new: bool,
    pub shown: u64,
}

/// The `docs` of a `list` answer.
pub fn parse(machine: &str, answer: &Value) -> Vec<DocRow> {
    answer["docs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| {
            Some(DocRow {
                machine: machine.into(),
                id: d["id"].as_str()?.into(),
                project: d["project"].as_str()?.into(),
                // Untrusted text from another computer: no control characters on our screen.
                title: d["title"]
                    .as_str()?
                    .chars()
                    .filter(|c| !c.is_control())
                    .collect(),
                by: d["by"].as_str().map(String::from),
                new: d["new"].as_bool().unwrap_or(false),
                shown: d["shown"].as_u64().unwrap_or(0),
            })
        })
        .collect()
}

/// The documents in `now` that are new and were not new in `before`: not listed, already read,
/// or shown again since. A computer seen for the first time announces nothing.
pub fn arrivals<'a>(before: &[DocRow], now: &'a [DocRow], watched: bool) -> Vec<&'a DocRow> {
    if !watched {
        return vec![];
    }
    now.iter()
        .filter(|d| d.new)
        .filter(|d| {
            !before
                .iter()
                .any(|b| b.id == d.id && b.new && b.shown == d.shown)
        })
        .collect()
}

/// `s` cut to `cells` terminal cells, counting a character outside ASCII as two (wide letters
/// are; accents are counted too wide, which only leaves a little room).
pub fn fit_cells(s: &str, cells: usize) -> String {
    let width = |c: char| if c.is_ascii() { 1 } else { 2 };
    if s.chars().map(width).sum::<usize>() <= cells {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        if used + width(c) >= cells {
            break;
        }
        used += width(c);
        out.push(c);
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(id: &str, new: bool, shown: u64) -> DocRow {
        DocRow {
            machine: String::new(),
            id: id.into(),
            project: "p".into(),
            title: id.into(),
            by: Some("p-main".into()),
            new,
            shown,
        }
    }

    #[test]
    fn rows_come_from_the_list_answer() {
        let answer = json!({"docs": [
            {"id": "a", "project": "p", "title": "A", "by": "p-main", "new": true, "shown": 7},
            {"id": "b", "project": "p", "title": "B", "new": false},
            {"id": "broken"},
        ]});
        let rows = parse("m", &answer);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            (rows[0].machine.as_str(), rows[0].by.as_deref(), rows[0].new),
            ("m", Some("p-main"), true)
        );
        assert_eq!((rows[1].by.as_deref(), rows[1].new), (None, false));
        assert!(parse("m", &json!({})).is_empty());
    }

    #[test]
    fn titles_are_cut_to_the_cells_there_are() {
        assert_eq!(fit_cells("Plan", 10), "Plan");
        assert_eq!(fit_cells("abcdefghij", 6), "abcde…");
        // Four wide letters need eight cells; in six, two fit and the cut mark.
        assert_eq!(fit_cells("한글한글", 6), "한글…");
        assert_eq!(fit_cells("한글", 4), "한글");
    }

    #[test]
    fn only_a_document_that_became_new_is_announced() {
        let before = [row("a", true, 1), row("b", false, 1)];
        let now = [
            row("a", true, 1),
            row("b", true, 2),
            row("c", true, 3),
            row("d", false, 3),
        ];
        let ids: Vec<_> = arrivals(&before, &now, true)
            .iter()
            .map(|d| d.id.as_str())
            .collect();
        assert_eq!(ids, ["b", "c"], "a is unchanged, d is read");
        // Shown again while still unread: the person is told again.
        assert_eq!(
            arrivals(&[row("a", true, 1)], &[row("a", true, 5)], true).len(),
            1
        );
        assert!(
            arrivals(&before, &now, false).is_empty(),
            "first sight of a computer"
        );
    }
}

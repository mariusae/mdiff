#[derive(Debug, Default)]
pub struct Document {
    pub items: Vec<Item>,
}

#[derive(Debug)]
pub enum Item {
    Meta(String),
    Hunk(Hunk),
}

#[derive(Debug)]
pub struct Hunk {
    pub header: String,
    pub rows: Vec<Row>,
}

#[derive(Debug, Eq, PartialEq)]
pub enum Row {
    Context(String),
    Delete(String),
    Insert(String),
    Change { old: String, new: String },
    Annotation(String),
}

enum RawRow {
    Context(String),
    Delete(String),
    Insert(String),
    Annotation(String),
}

pub fn parse(input: &str) -> Document {
    let mut items = Vec::new();
    let mut current_header: Option<String> = None;
    let mut raw_rows: Vec<RawRow> = Vec::new();

    for line in input.lines() {
        if line.starts_with("@@") {
            flush_hunk(&mut items, &mut current_header, &mut raw_rows);
            current_header = Some(line.to_owned());
            continue;
        }

        if current_header.is_some() {
            match line.chars().next() {
                Some(' ') => {
                    raw_rows.push(RawRow::Context(line[1..].to_owned()));
                    continue;
                }
                Some('-') => {
                    raw_rows.push(RawRow::Delete(line[1..].to_owned()));
                    continue;
                }
                Some('+') => {
                    raw_rows.push(RawRow::Insert(line[1..].to_owned()));
                    continue;
                }
                Some('\\') => {
                    raw_rows.push(RawRow::Annotation(line.to_owned()));
                    continue;
                }
                _ => {
                    flush_hunk(&mut items, &mut current_header, &mut raw_rows);
                }
            }
        }

        items.push(Item::Meta(line.to_owned()));
    }

    flush_hunk(&mut items, &mut current_header, &mut raw_rows);

    Document { items }
}

fn flush_hunk(items: &mut Vec<Item>, header: &mut Option<String>, raw_rows: &mut Vec<RawRow>) {
    let Some(header) = header.take() else {
        raw_rows.clear();
        return;
    };

    let rows = build_rows(std::mem::take(raw_rows));
    items.push(Item::Hunk(Hunk { header, rows }));
}

fn build_rows(raw_rows: Vec<RawRow>) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut deleted = Vec::new();
    let mut inserted = Vec::new();

    let flush_change_block =
        |rows: &mut Vec<Row>, deleted: &mut Vec<String>, inserted: &mut Vec<String>| {
            let len = deleted.len().max(inserted.len());
            for index in 0..len {
                match (deleted.get(index), inserted.get(index)) {
                    (Some(old), Some(new)) => rows.push(Row::Change {
                        old: old.clone(),
                        new: new.clone(),
                    }),
                    (Some(old), None) => rows.push(Row::Delete(old.clone())),
                    (None, Some(new)) => rows.push(Row::Insert(new.clone())),
                    (None, None) => {}
                }
            }

            deleted.clear();
            inserted.clear();
        };

    for row in raw_rows {
        match row {
            RawRow::Context(line) => {
                flush_change_block(&mut rows, &mut deleted, &mut inserted);
                rows.push(Row::Context(line));
            }
            RawRow::Delete(line) => deleted.push(line),
            RawRow::Insert(line) => inserted.push(line),
            RawRow::Annotation(line) => {
                flush_change_block(&mut rows, &mut deleted, &mut inserted);
                rows.push(Row::Annotation(line));
            }
        }
    }

    flush_change_block(&mut rows, &mut deleted, &mut inserted);
    rows
}

#[cfg(test)]
mod tests {
    use super::Item;
    use super::Row;
    use super::parse;

    #[test]
    fn parses_context_and_change_rows() {
        let input = "\
diff --git a/a.txt b/a.txt
@@ -1,2 +1,2 @@
-old
+new
 same
";

        let doc = parse(input);
        assert_eq!(doc.items.len(), 2);

        let Item::Hunk(hunk) = &doc.items[1] else {
            panic!("expected hunk");
        };

        assert_eq!(
            hunk.rows,
            vec![
                Row::Change {
                    old: "old".into(),
                    new: "new".into(),
                },
                Row::Context("same".into()),
            ]
        );
    }

    #[test]
    fn keeps_annotations_separate() {
        let input = "\
@@ -1 +1 @@
-before
+after
\\ No newline at end of file
";

        let doc = parse(input);
        let Item::Hunk(hunk) = &doc.items[0] else {
            panic!("expected hunk");
        };

        assert_eq!(
            hunk.rows,
            vec![
                Row::Change {
                    old: "before".into(),
                    new: "after".into(),
                },
                Row::Annotation("\\ No newline at end of file".into()),
            ]
        );
    }
}

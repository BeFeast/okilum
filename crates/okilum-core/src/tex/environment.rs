//! Bounded TeX alignment environments. Separators belong to the current
//! environment only; groups and nested environments consume their own tokens.
use super::*;

const MAX_COLUMNS: usize = 32;
const MAX_ROWS: usize = 64;
const MAX_CELLS: usize = 256;

impl Parser<'_> {
    pub(super) fn environment(&mut self) -> Result<Node, Error> {
        let name = self.environment_name()?;
        let mut columns = match name.as_str() {
            "array" => {
                let spec = self.environment_name()?;
                let columns: Vec<_> = spec
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .map(|c| match c {
                        'l' => Ok(Alignment::Left),
                        'c' => Ok(Alignment::Center),
                        'r' => Ok(Alignment::Right),
                        _ => error("Array columns must use l, c or r"),
                    })
                    .collect::<Result<_, _>>()?;
                if columns.is_empty() || columns.len() > MAX_COLUMNS {
                    return error("Array needs 1 to 32 columns");
                }
                columns
            }
            "equation" | "equation*" | "gathered" | "gather" | "gather*" => {
                vec![Alignment::Center]
            }
            "cases" => vec![Alignment::Left; 2],
            "matrix" | "pmatrix" | "bmatrix" | "Bmatrix" | "vmatrix" | "Vmatrix" | "aligned"
            | "align" | "align*" | "split" | "eqnarray" | "eqnarray*" => Vec::new(),
            _ => return error(format!("Unsupported math environment {name}")),
        };
        let fixed_columns = !columns.is_empty();
        let mut rows = Vec::new();
        let mut cells = Vec::new();
        let mut count = 0;
        loop {
            cells.push(self.row(End::Cell)?);
            count += 1;
            if count > MAX_CELLS || cells.len() > MAX_COLUMNS || rows.len() >= MAX_ROWS {
                return error("Math environment is too large");
            }
            if fixed_columns && cells.len() > columns.len() {
                return error("Too many cells for math environment columns");
            }
            if self.peek_char() == Some('&') {
                self.chars.next();
                continue;
            }
            if self.at_command("end") {
                self.next_token();
                let closed = self.environment_name()?;
                if closed != name {
                    return error(format!("Expected \\end{{{name}}}, found \\end{{{closed}}}"));
                }
                // A trailing row separator does not add an empty final row.
                if !cells.iter().all(Vec::is_empty) || rows.is_empty() {
                    rows.push(cells);
                }
                break;
            }
            self.next_token(); // \\ or \cr, already recognized by row(End::Cell).
            rows.push(std::mem::take(&mut cells));
        }
        if !fixed_columns {
            let width = rows.iter().map(Vec::len).max().unwrap_or(1);
            let aligned = matches!(name.as_str(), "aligned" | "align" | "align*" | "split");
            columns = (0..width)
                .map(|i| match (aligned, i % 2) {
                    _ if name.starts_with("eqnarray") => match i {
                        0 => Alignment::Right,
                        i if i % 2 == 1 => Alignment::Center,
                        _ => Alignment::Left,
                    },
                    (true, 0) => Alignment::Right,
                    (true, _) => Alignment::Left,
                    _ => Alignment::Center,
                })
                .collect();
        }
        let body = Node::Array { rows, columns };
        let (open, close) = match name.as_str() {
            "pmatrix" => ("(", ")"),
            "bmatrix" => ("[", "]"),
            "Bmatrix" => ("{", "}"),
            "vmatrix" => ("|", "|"),
            "Vmatrix" => ("‖", "‖"),
            "cases" => ("{", ""),
            _ => return Ok(body),
        };
        Ok(Node::Fenced {
            open: open.into(),
            close: close.into(),
            body: vec![body],
        })
    }

    fn environment_name(&mut self) -> Result<String, Error> {
        if self.peek_char() != Some('{') {
            return error("Math environment needs a braced name or column specification");
        }
        self.text_argument()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn math_2684_reported_arrays_and_eqnarray_parse() {
        for separator in [r"\\", r"\cr"] {
            let source = format!(
                r"\begin{{array}}{{c}} y_1 {separator} y_2 \mathtt{{t}}_i {separator} z_{{3,4}} \end{{array}}"
            );
            let nodes = parse(&source).unwrap();
            let [Node::Array { rows, columns }] = nodes.as_slice() else {
                panic!("expected an array: {nodes:?}");
            };
            assert_eq!(rows.len(), 3);
            assert_eq!(columns, &[Alignment::Center]);
            assert!(rows.iter().all(|r| r.len() == 1 && !r[0].is_empty()));
        }
        let nodes = parse(
            r"\begin{eqnarray} x' &=& &x \sin\phi &+& z \cos\phi \\ z' &=& - &x \cos\phi &+& z \sin\phi \\ \end{eqnarray}",
        ).unwrap();
        let [Node::Array { rows, columns }] = nodes.as_slice() else {
            panic!()
        };
        assert_eq!(rows.len(), 2);
        assert_eq!(
            columns,
            &[
                Alignment::Right,
                Alignment::Center,
                Alignment::Left,
                Alignment::Center,
                Alignment::Left
            ]
        );
        assert!(rows.iter().all(|r| r.len() == 5));
    }

    #[test]
    fn nested_environments_keep_cells_styles_and_linear_boundaries() {
        let source = r"\mathbf{\begin{pmatrix} a & \frac{1}{2} \\ \begin{array}{lr} x & y \end{array} & b \end{pmatrix}}";
        assert!(parse(source).is_ok());
        let rendered = linear(source).unwrap();
        assert!(rendered.text.contains("a, ½; [x, y], b"), "{rendered:?}");
        assert!(!rendered.runs.is_empty());
    }

    #[test]
    fn malformed_and_excessive_environments_stay_errors() {
        for source in [
            r"\begin{array}{cc} a & b",
            r"\begin{array}{c} a \end{matrix}",
            r"\begin{array}{c} a & b \end{array}",
            r"\begin{array}{q} a \end{array}",
            r"\begin{unknown} x \end{unknown}",
            r"\end{array}",
            r"a & b",
            r"\begin{array}{c} {a & b} \end{array}",
        ] {
            assert!(parse(source).is_err(), "{source}");
        }
        let deep = format!(
            "{}x{}",
            r"\begin{matrix}".repeat(30),
            r"\end{matrix}".repeat(30)
        );
        assert!(parse(&deep).is_err());
        let wide = format!(
            r"\begin{{matrix}} {} \end{{matrix}}",
            vec!["x"; 33].join(" & ")
        );
        assert!(parse(&wide).is_err());
    }
}

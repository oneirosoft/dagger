use std::io;
use std::io::Write;

use crate::core::restack::RestackPreview;

pub fn print_trimmed_stderr(output: Option<&str>) {
    if let Some(trimmed) = output.map(str::trim).filter(|trimmed| !trimmed.is_empty()) {
        eprintln!("{trimmed}");
    }
}

pub fn print_restack_pause_guidance(output: Option<&str>) {
    print_trimmed_stderr(output);
    eprintln!("Resolve the rebase conflicts, stage the changes, and run 'dgr sync --continue'.");
    eprintln!("If you abort the rebase, rerun the original dgr command from the start.");
}

pub fn confirm_yes_no(prompt: &str) -> io::Result<bool> {
    let mut stdout = io::stdout();
    write!(stdout, "{prompt}")?;
    stdout.flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    writeln!(stdout)?;
    stdout.flush()?;

    Ok(matches!(input.trim(), "y" | "Y" | "yes" | "YES" | "Yes"))
}

pub fn format_restacked_branches(branches: &[RestackPreview]) -> String {
    let mut lines = vec!["Restacked:".to_string()];

    for branch in branches {
        lines.push(format!(
            "- {} onto {}",
            branch.branch_name, branch.onto_branch
        ));
    }

    lines.join("\n")
}

pub fn join_sections(sections: &[String]) -> String {
    sections.join("\n\n")
}

pub fn render_tree<T, FLabel, FChildren>(
    root_label: Option<String>,
    roots: &[T],
    format_label: &FLabel,
    children_of: &FChildren,
) -> String
where
    FLabel: Fn(&T) -> String,
    FChildren: for<'a> Fn(&'a T) -> &'a [T],
{
    let mut lines = Vec::new();

    if let Some(root_label) = root_label {
        lines.push(root_label);
    }

    append_tree_nodes(&mut lines, roots, format_label, children_of);

    lines.join("\n")
}

pub fn append_tree_nodes<T, FLabel, FChildren>(
    lines: &mut Vec<String>,
    nodes: &[T],
    format_label: &FLabel,
    children_of: &FChildren,
) where
    FLabel: Fn(&T) -> String,
    FChildren: for<'a> Fn(&'a T) -> &'a [T],
{
    for (index, node) in nodes.iter().enumerate() {
        render_tree_node(
            node,
            "",
            index + 1 == nodes.len(),
            lines,
            format_label,
            children_of,
        );
    }
}

fn render_tree_node<T, FLabel, FChildren>(
    node: &T,
    prefix: &str,
    is_last: bool,
    lines: &mut Vec<String>,
    format_label: &FLabel,
    children_of: &FChildren,
) where
    FLabel: Fn(&T) -> String,
    FChildren: for<'a> Fn(&'a T) -> &'a [T],
{
    let connector = if is_last { "└──" } else { "├──" };
    lines.push(format!("{prefix}{connector} {}", format_label(node)));

    let child_prefix = if is_last {
        format!("{prefix}    ")
    } else {
        format!("{prefix}│   ")
    };

    let children = children_of(node);
    for (index, child) in children.iter().enumerate() {
        render_tree_node(
            child,
            &child_prefix,
            index + 1 == children.len(),
            lines,
            format_label,
            children_of,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{format_restacked_branches, join_sections, render_tree};
    use crate::core::restack::RestackPreview;

    #[derive(Debug)]
    struct Node {
        label: &'static str,
        children: Vec<Node>,
    }

    #[test]
    fn formats_restacked_branches_section() {
        let rendered = format_restacked_branches(&[
            RestackPreview {
                branch_name: "feat/auth-api".into(),
                onto_branch: "feat/auth".into(),
                parent_changed: false,
            },
            RestackPreview {
                branch_name: "feat/auth-ui".into(),
                onto_branch: "main".into(),
                parent_changed: true,
            },
        ]);

        assert_eq!(
            rendered,
            concat!(
                "Restacked:\n",
                "- feat/auth-api onto feat/auth\n",
                "- feat/auth-ui onto main"
            )
        );
    }

    #[test]
    fn joins_sections_with_blank_lines() {
        assert_eq!(
            join_sections(&["one".into(), "two".into(), "three".into()]),
            "one\n\ntwo\n\nthree"
        );
    }

    #[test]
    fn renders_tree_lines_for_shared_cli_views() {
        let rendered = render_tree(
            Some("main".into()),
            &[Node {
                label: "feat/auth",
                children: vec![
                    Node {
                        label: "feat/auth-api",
                        children: vec![],
                    },
                    Node {
                        label: "feat/auth-ui",
                        children: vec![],
                    },
                ],
            }],
            &|node| node.label.to_string(),
            &|node| node.children.as_slice(),
        );

        assert_eq!(
            rendered,
            concat!(
                "main\n",
                "└── feat/auth\n",
                "    ├── feat/auth-api\n",
                "    └── feat/auth-ui"
            )
        );
    }
}

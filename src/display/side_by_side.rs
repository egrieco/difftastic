//! Side-by-side (two column) display of diffs.

use cansi::{self, categorise_text};
use owo_colors::{OwoColorize, Style};
use rustc_hash::FxHashMap;
use std::{cmp::max, collections::HashSet};
use yansi::{Color, Paint};

use crate::{
    constants::Side,
    display::context::all_matched_lines_filled,
    display::hunks::{matched_lines_for_hunk, Hunk},
    display::style::{
        self, apply_colors, color_positions, novel_style, split_and_apply, BackgroundColor,
    },
    lines::{codepoint_len, format_line_num, LineNumber},
    options::{DisplayMode, DisplayOptions},
    parse::syntax::{zip_pad_shorter, MatchedPos},
    positions::SingleLineSpan,
};

const SPACER: &str = " ";

/// Split `s` on \n or \r\n. Always returns a non-empty vec.
///
/// This differs from `str::lines`, which considers `""` to be zero
/// lines and `"foo\n"` to be one line.
fn split_on_newlines(s: &str) -> Vec<&str> {
    s.split('\n')
        .map(|l| {
            if let Some(l) = l.strip_suffix('\r') {
                l
            } else {
                l
            }
        })
        .collect()
}

fn format_line_num_padded(line_num: LineNumber, column_width: usize) -> String {
    format!(
        "{:width$} ",
        line_num.one_indexed(),
        width = column_width - 1
    )
}

fn format_missing_line_num(
    prev_num: LineNumber,
    source_dims: &SourceDimensions,
    is_lhs: bool,
    use_color: bool,
) -> String {
    let column_width = if is_lhs {
        source_dims.lhs_line_nums_width
    } else {
        source_dims.rhs_line_nums_width
    };

    let after_end = if is_lhs {
        prev_num >= source_dims.lhs_max_line
    } else {
        prev_num >= source_dims.rhs_max_line
    };

    let mut style = Style::new();
    if use_color {
        style = style.dimmed();
    }

    let num_digits = format!("{}", prev_num.one_indexed()).len();
    format!(
        "{:>width$} ",
        (if after_end { " " } else { "." }).repeat(num_digits),
        width = column_width - 1
    )
    .style(style)
    .to_string()
}

/// Display `src` in a single column (e.g. a file removal or addition).
fn display_single_column(
    lhs_display_path: &str,
    rhs_display_path: &str,
    lang_name: &str,
    src: &str,
    is_lhs: bool,
    display_options: &DisplayOptions,
) -> String {
    let column_width = format_line_num(src.lines().count().into()).len();

    let mut result = String::with_capacity(src.len());
    result.push_str(&style::header(
        lhs_display_path,
        rhs_display_path,
        1,
        1,
        lang_name,
        display_options,
    ));
    result.push('\n');

    let mut style = Style::new();
    if display_options.use_color {
        style = novel_style(Style::new(), is_lhs, display_options.background_color);
    }

    for (i, line) in src.lines().enumerate() {
        result.push_str(
            &format_line_num_padded(i.into(), column_width)
                .style(style)
                .to_string(),
        );
        result.push_str(line);
        result.push('\n');
    }

    result
}

fn display_line_nums(
    lhs_line_num: Option<LineNumber>,
    rhs_line_num: Option<LineNumber>,
    source_dims: &SourceDimensions,
    use_color: bool,
    background: BackgroundColor,
    lhs_has_novel: bool,
    rhs_has_novel: bool,
    prev_lhs_line_num: Option<LineNumber>,
    prev_rhs_line_num: Option<LineNumber>,
) -> (String, String) {
    let display_lhs_line_num: String = match lhs_line_num {
        Some(line_num) => {
            let s = format_line_num_padded(line_num, source_dims.lhs_line_nums_width);
            if lhs_has_novel && use_color {
                // TODO: factor out applying colours to line numbers.
                if background.is_dark() {
                    s.bright_red().to_string()
                } else {
                    s.red().to_string()
                }
            } else {
                s
            }
        }
        None => format_missing_line_num(
            prev_lhs_line_num.unwrap_or_else(|| 1.into()),
            source_dims,
            true,
            use_color,
        ),
    };
    let display_rhs_line_num: String = match rhs_line_num {
        Some(line_num) => {
            let s = format_line_num_padded(line_num, source_dims.rhs_line_nums_width);
            if rhs_has_novel && use_color {
                if background.is_dark() {
                    s.bright_green().to_string()
                } else {
                    s.green().to_string()
                }
            } else {
                s
            }
        }
        None => format_missing_line_num(
            prev_rhs_line_num.unwrap_or_else(|| 1.into()),
            source_dims,
            false,
            use_color,
        ),
    };

    (display_lhs_line_num, display_rhs_line_num)
}

// Sizes used when displaying a hunk.
struct SourceDimensions {
    lhs_content_width: usize,
    rhs_content_width: usize,
    lhs_line_nums_width: usize,
    rhs_line_nums_width: usize,
    lhs_max_line: LineNumber,
    rhs_max_line: LineNumber,
}

impl SourceDimensions {
    fn new(
        terminal_width: usize,
        line_nums: &[(Option<LineNumber>, Option<LineNumber>)],
        lhs_lines: &[&str],
        rhs_lines: &[&str],
    ) -> Self {
        let mut lhs_max_line: LineNumber = 1.into();
        let mut rhs_max_line: LineNumber = 1.into();
        let mut lhs_max_content = 1;
        let mut rhs_max_content = 1;

        for (lhs_line_num, rhs_line_num) in line_nums {
            if let Some(lhs_line_num) = lhs_line_num {
                lhs_max_line = max(lhs_max_line, *lhs_line_num);
                lhs_max_content = max(lhs_max_content, codepoint_len(lhs_lines[lhs_line_num.0]));
            }
            if let Some(rhs_line_num) = rhs_line_num {
                rhs_max_line = max(rhs_max_line, *rhs_line_num);
                rhs_max_content = max(rhs_max_content, codepoint_len(rhs_lines[rhs_line_num.0]));
            }
        }

        let lhs_line_nums_width = format_line_num(lhs_max_line).len();
        let rhs_line_nums_width = format_line_num(rhs_max_line).len();

        let lhs_total_width = (terminal_width - SPACER.len()) / 2;
        let lhs_content_width = lhs_total_width - lhs_line_nums_width;
        let rhs_content_width =
            terminal_width - lhs_total_width - SPACER.len() - rhs_line_nums_width;

        Self {
            lhs_content_width,
            rhs_content_width,
            lhs_line_nums_width,
            rhs_line_nums_width,
            lhs_max_line,
            rhs_max_line,
        }
    }
}

pub fn lines_with_novel(
    lhs_mps: &[MatchedPos],
    rhs_mps: &[MatchedPos],
) -> (HashSet<LineNumber>, HashSet<LineNumber>) {
    let lhs_lines_with_novel: HashSet<LineNumber> = lhs_mps
        .iter()
        .filter(|mp| mp.kind.is_novel())
        .map(|mp| mp.pos.line)
        .collect();
    let rhs_lines_with_novel: HashSet<LineNumber> = rhs_mps
        .iter()
        .filter(|mp| mp.kind.is_novel())
        .map(|mp| mp.pos.line)
        .collect();

    (lhs_lines_with_novel, rhs_lines_with_novel)
}

/// Calculate positions of highlights on both sides. This includes
/// both syntax highlighting and added/removed content highlighting.
fn highlight_positions(
    background: BackgroundColor,
    syntax_highlight: bool,
    lhs_mps: &[MatchedPos],
    rhs_mps: &[MatchedPos],
) -> (
    FxHashMap<LineNumber, Vec<(SingleLineSpan, Style)>>,
    FxHashMap<LineNumber, Vec<(SingleLineSpan, Style)>>,
) {
    let lhs_positions = color_positions(true, background, syntax_highlight, lhs_mps);
    // Preallocate the hashmap assuming the average line will have 2 items on it.
    let mut lhs_styles: FxHashMap<LineNumber, Vec<(SingleLineSpan, Style)>> = FxHashMap::default();
    for (span, style) in lhs_positions {
        let styles = lhs_styles.entry(span.line).or_insert_with(Vec::new);
        styles.push((span, style));
    }

    let rhs_positions = color_positions(false, background, syntax_highlight, rhs_mps);
    let mut rhs_styles: FxHashMap<LineNumber, Vec<(SingleLineSpan, Style)>> = FxHashMap::default();
    for (span, style) in rhs_positions {
        let styles = rhs_styles.entry(span.line).or_insert_with(Vec::new);
        styles.push((span, style));
    }

    (lhs_styles, rhs_styles)
}

fn highlight_as_novel(
    line_num: Option<LineNumber>,
    lines: &[&str],
    opposite_line_num: Option<LineNumber>,
    lines_with_novel: &HashSet<LineNumber>,
) -> bool {
    if let Some(line_num) = line_num {
        // If this line contains any novel tokens, highlight it.
        if lines_with_novel.contains(&line_num) {
            return true;
        }

        let line_content = lines.get(line_num.0).map(|s| str::trim(s));
        // If this is a blank line without a corresponding line on the
        // other side, highlight it too. This helps highlight novel
        // blank lines.
        if line_content == Some("") && opposite_line_num.is_none() {
            return true;
        }
    }

    false
}

pub fn print(
    hunks: &[Hunk],
    display_options: &DisplayOptions,
    lhs_display_path: &str,
    rhs_display_path: &str,
    lang_name: &str,
    lhs_src: &str,
    rhs_src: &str,
    lhs_mps: &[MatchedPos],
    rhs_mps: &[MatchedPos],
) {
    let (lhs_colored_src, rhs_colored_src) = if display_options.use_color {
        (
            apply_colors(
                lhs_src,
                true,
                display_options.syntax_highlight,
                display_options.background_color,
                lhs_mps,
            ),
            apply_colors(
                rhs_src,
                false,
                display_options.syntax_highlight,
                display_options.background_color,
                rhs_mps,
            ),
        )
    } else {
        (lhs_src.to_string(), rhs_src.to_string())
    };

    if lhs_src.is_empty() {
        println!(
            "{}",
            display_single_column(
                lhs_display_path,
                rhs_display_path,
                lang_name,
                &rhs_colored_src,
                false,
                display_options
            )
        );
        return;
    }
    if rhs_src.is_empty() {
        println!(
            "{}",
            display_single_column(
                lhs_display_path,
                rhs_display_path,
                lang_name,
                &lhs_colored_src,
                true,
                display_options
            )
        );
        return;
    }

    // TODO: this is largely duplicating the `apply_colors` logic.
    let (lhs_highlights, rhs_highlights) = if display_options.use_color {
        highlight_positions(
            display_options.background_color,
            display_options.syntax_highlight,
            lhs_mps,
            rhs_mps,
        )
    } else {
        (FxHashMap::default(), FxHashMap::default())
    };

    let lhs_lines = split_on_newlines(lhs_src);
    let rhs_lines = split_on_newlines(rhs_src);
    let lhs_colored_lines = split_on_newlines(&lhs_colored_src);
    let rhs_colored_lines = split_on_newlines(&rhs_colored_src);

    let (lhs_lines_with_novel, rhs_lines_with_novel) = lines_with_novel(lhs_mps, rhs_mps);

    let mut prev_lhs_line_num = None;
    let mut prev_rhs_line_num = None;

    let matched_lines = all_matched_lines_filled(lhs_mps, rhs_mps, &lhs_lines, &rhs_lines);

    for (i, hunk) in hunks.iter().enumerate() {
        println!(
            "{}",
            style::header(
                lhs_display_path,
                rhs_display_path,
                i + 1,
                hunks.len(),
                lang_name,
                display_options
            )
        );

        let aligned_lines = matched_lines_for_hunk(&matched_lines, hunk);
        let no_lhs_changes = hunk.novel_lhs.is_empty();
        let no_rhs_changes = hunk.novel_rhs.is_empty();
        let same_lines = aligned_lines.iter().all(|(l, r)| l == r);

        let source_dims = SourceDimensions::new(
            display_options.display_width,
            &aligned_lines,
            &lhs_lines,
            &rhs_lines,
        );
        for (lhs_line_num, rhs_line_num) in aligned_lines {
            let lhs_line_novel = highlight_as_novel(
                lhs_line_num,
                &lhs_lines,
                rhs_line_num,
                &lhs_lines_with_novel,
            );
            let rhs_line_novel = highlight_as_novel(
                rhs_line_num,
                &rhs_lines,
                lhs_line_num,
                &rhs_lines_with_novel,
            );

            let (display_lhs_line_num, display_rhs_line_num) = display_line_nums(
                lhs_line_num,
                rhs_line_num,
                &source_dims,
                display_options.use_color,
                display_options.background_color,
                lhs_line_novel,
                rhs_line_novel,
                prev_lhs_line_num,
                prev_rhs_line_num,
            );

            let show_both = matches!(
                display_options.display_mode,
                DisplayMode::SideBySideShowBoth
            );
            if no_lhs_changes && !show_both {
                match rhs_line_num {
                    Some(rhs_line_num) => {
                        let rhs_line = &rhs_colored_lines[rhs_line_num.0];
                        let line_to_print = if same_lines {
                            format!("{}{}", display_rhs_line_num, rhs_line)
                        } else {
                            format!(
                                "{}{}{}",
                                display_lhs_line_num, display_rhs_line_num, rhs_line
                            )
                        };
                        let (line_bg, padding_len) = if rhs_lines_with_novel.contains(&rhs_line_num)
                        {
                            (
                                Color::Fixed(194),
                                display_options.display_width
                                // we are using cansi::categorize_text to remove ANSI escapes
                                // if we don't do this, we can't properly pad the line length
                                // tried several other ANSI stripping libs, this one actually works
                                    - categorise_text(&line_to_print)
                                        .iter()
                                        .map(|s| (s.end - s.start) as usize)
                                        .sum::<usize>(),
                            )
                        } else {
                            (Color::Default, 0)
                        };
                        println!(
                            "{}",
                            Paint::wrapping(format!(
                                "{}{}",
                                line_to_print,
                                " ".repeat(padding_len)
                            ))
                            .bg(line_bg)
                        );
                    }
                    None => {
                        // We didn't have any changed RHS lines in the
                        // hunk, but we had some contextual lines that
                        // only occurred on the LHS (e.g. extra newlines).
                        println!("{}{}", display_rhs_line_num, display_rhs_line_num);
                    }
                }
            } else if no_rhs_changes && !show_both {
                match lhs_line_num {
                    Some(lhs_line_num) => {
                        let lhs_line = &lhs_colored_lines[lhs_line_num.0];
                        let line_to_print = if same_lines {
                            format!("{}{}", display_lhs_line_num, lhs_line)
                        } else {
                            format!(
                                "{}{}{}",
                                display_lhs_line_num, display_lhs_line_num, lhs_line
                            )
                        };
                        let (line_bg, padding_len) = if lhs_lines_with_novel.contains(&lhs_line_num)
                        {
                            (
                                Color::Fixed(224),
                                display_options.display_width
                                // we are using cansi::categorize_text to remove ANSI escapes
                                // if we don't do this, we can't properly pad the line length
                                // tried several other ANSI stripping libs, this one actually works
                                    - categorise_text(&line_to_print)
                                        .iter()
                                        .map(|s| (s.end - s.start) as usize)
                                        .sum::<usize>(),
                            )
                        } else {
                            (Color::Default, 0)
                        };
                        println!(
                            "{}",
                            Paint::wrapping(format!(
                                "{}{}",
                                line_to_print,
                                " ".repeat(padding_len)
                            ))
                            .bg(line_bg)
                        );
                    }
                    None => {
                        println!("{}{}", display_lhs_line_num, display_lhs_line_num);
                    }
                }
            } else {
                let lhs_line = match lhs_line_num {
                    Some(lhs_line_num) => split_and_apply(
                        lhs_lines[lhs_line_num.0],
                        source_dims.lhs_content_width,
                        display_options.use_color,
                        lhs_highlights.get(&lhs_line_num).unwrap_or(&vec![]),
                        Side::Left,
                    ),
                    None => vec![" ".repeat(source_dims.lhs_content_width)],
                };
                let rhs_line = match rhs_line_num {
                    Some(rhs_line_num) => split_and_apply(
                        rhs_lines[rhs_line_num.0],
                        source_dims.rhs_content_width,
                        display_options.use_color,
                        rhs_highlights.get(&rhs_line_num).unwrap_or(&vec![]),
                        Side::Right,
                    ),
                    None => vec!["".into()],
                };

                for (i, (lhs_line, rhs_line)) in zip_pad_shorter(&lhs_line, &rhs_line)
                    .into_iter()
                    .enumerate()
                {
                    let lhs_line =
                        lhs_line.unwrap_or_else(|| " ".repeat(source_dims.lhs_content_width));
                    let rhs_line = rhs_line.unwrap_or_else(|| "".into());
                    let lhs_num: String = if i == 0 {
                        display_lhs_line_num.clone()
                    } else {
                        let mut s = format_missing_line_num(
                            lhs_line_num
                                .unwrap_or_else(|| prev_lhs_line_num.unwrap_or_else(|| 10.into())),
                            &source_dims,
                            true,
                            display_options.use_color,
                        );
                        if let Some(line_num) = lhs_line_num {
                            if lhs_lines_with_novel.contains(&line_num) {
                                s = if display_options.background_color.is_dark() {
                                    s.bright_red().to_string()
                                } else {
                                    s.red().to_string()
                                };
                            }
                        }
                        s
                    };
                    let rhs_num: String = if i == 0 {
                        display_rhs_line_num.clone()
                    } else {
                        let mut s = format_missing_line_num(
                            rhs_line_num
                                .unwrap_or_else(|| prev_rhs_line_num.unwrap_or_else(|| 10.into())),
                            &source_dims,
                            false,
                            display_options.use_color,
                        );
                        if let Some(line_num) = rhs_line_num {
                            if rhs_lines_with_novel.contains(&line_num) {
                                s = if display_options.background_color.is_dark() {
                                    s.bright_green().to_string()
                                } else {
                                    s.green().to_string()
                                };
                            }
                        }
                        s
                    };

                    println!(
                        "{}{}{}",
                        Paint::wrapping(format!("{}{}", lhs_num, lhs_line)).bg(
                            if lhs_line_num.is_some()
                                && lhs_lines_with_novel.contains(&lhs_line_num.unwrap())
                            {
                                Color::Fixed(224)
                            } else {
                                Color::Default
                            }
                        ),
                        SPACER,
                        Paint::wrapping(format!("{}{}", rhs_num, rhs_line)).bg(
                            if rhs_line_num.is_some()
                                && rhs_lines_with_novel.contains(&rhs_line_num.unwrap())
                            {
                                Color::Fixed(194)
                            } else {
                                Color::Default
                            }
                        ),
                    );
                }
            }

            if lhs_line_num.is_some() {
                prev_lhs_line_num = lhs_line_num;
            }
            if rhs_line_num.is_some() {
                prev_rhs_line_num = rhs_line_num;
            }
        }
        println!();
    }
}

#[cfg(test)]
mod tests {
    use crate::syntax::{AtomKind, MatchKind, TokenKind};

    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn test_width_calculations() {
        let line_nums = [(Some(1.into()), Some(10.into()))];
        let source_dims = SourceDimensions::new(
            80,
            &line_nums,
            &split_on_newlines("foo\nbar\n"),
            &split_on_newlines("x\nx\nx\nx\nx\nx\nx\nx\nx\nx\nx\n"),
        );

        assert_eq!(source_dims.lhs_line_nums_width, 2);
        assert_eq!(source_dims.rhs_line_nums_width, 3);
    }

    #[test]
    fn test_format_missing_line_num() {
        let source_dims = SourceDimensions::new(
            80,
            &[
                (Some(0.into()), Some(0.into())),
                (Some(1.into()), Some(1.into())),
            ],
            &split_on_newlines("foo\nbar\n"),
            &split_on_newlines("fox\nbax\n"),
        );

        assert_eq!(
            format_missing_line_num(0.into(), &source_dims, true, true),
            ". ".dimmed().to_string()
        );
        assert_eq!(
            format_missing_line_num(0.into(), &source_dims, true, false),
            ". ".to_string()
        );
    }

    #[test]
    fn test_format_missing_line_num_at_end() {
        let source_dims = SourceDimensions::new(
            80,
            &[
                (Some(0.into()), Some(0.into())),
                (Some(1.into()), Some(1.into())),
            ],
            &split_on_newlines("foo\nbar\n"),
            &split_on_newlines("fox\nbax\n"),
        );

        assert_eq!(
            format_missing_line_num(1.into(), &source_dims, true, true),
            "  ".dimmed().to_string()
        );
        assert_eq!(
            format_missing_line_num(1.into(), &source_dims, true, false),
            "  ".to_string()
        );
    }

    #[test]
    fn test_display_single_column() {
        let display_options = DisplayOptions {
            background_color: BackgroundColor::Dark,
            use_color: false,
            display_mode: DisplayMode::SideBySide,
            print_unchanged: true,
            tab_width: 8,
            display_width: 80,
            in_vcs: false,
            syntax_highlight: true,
        };

        // Basic smoke test.
        let res = display_single_column(
            "foo.py",
            "foo.py",
            "Python",
            "print(123)\n",
            false,
            &display_options,
        );
        assert!(res.len() > 10);
    }

    #[test]
    fn test_split_line_empty() {
        assert_eq!(split_on_newlines(""), vec![""]);
    }

    #[test]
    fn test_split_line_single() {
        assert_eq!(split_on_newlines("foo"), vec!["foo"]);
    }

    #[test]
    fn test_split_line_with_newline() {
        assert_eq!(split_on_newlines("foo\nbar"), vec!["foo", "bar"]);
    }

    #[test]
    fn test_split_line_with_crlf() {
        assert_eq!(split_on_newlines("foo\r\nbar"), vec!["foo", "bar"]);
    }

    #[test]
    fn test_split_line_with_trailing_newline() {
        assert_eq!(split_on_newlines("foo\nbar\n"), vec!["foo", "bar", ""]);
    }

    #[test]
    fn test_display_hunks() {
        // Simulate diffing:
        //
        // Old:
        // foo
        //
        // New:
        // bar
        let lhs_mps = [MatchedPos {
            kind: MatchKind::Novel {
                highlight: TokenKind::Atom(AtomKind::Normal),
            },
            pos: SingleLineSpan {
                line: 0.into(),
                start_col: 0,
                end_col: 3,
            },
        }];

        let rhs_mps = [MatchedPos {
            kind: MatchKind::Novel {
                highlight: TokenKind::Atom(AtomKind::Normal),
            },
            pos: SingleLineSpan {
                line: 0.into(),
                start_col: 0,
                end_col: 3,
            },
        }];

        let mut novel_lhs = HashSet::new();
        novel_lhs.insert(0.into());
        let mut novel_rhs = HashSet::new();
        novel_rhs.insert(0.into());

        let hunks = [Hunk {
            novel_lhs,
            novel_rhs,
            lines: vec![(Some(0.into()), Some(0.into()))],
        }];

        let display_options = DisplayOptions {
            background_color: BackgroundColor::Dark,
            use_color: true,
            display_mode: DisplayMode::SideBySide,
            print_unchanged: true,
            tab_width: 8,
            display_width: 80,
            syntax_highlight: true,
            in_vcs: true,
        };

        // Simple smoke test.
        print(
            &hunks,
            &display_options,
            "foo-old.el",
            "foo-new.el",
            "Emacs Lisp",
            "foo",
            "bar",
            &lhs_mps,
            &rhs_mps,
        );
    }
}

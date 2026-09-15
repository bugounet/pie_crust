//! Small native Markdown preview. Embedded HTML is shown as text and is never executed.

use eframe::egui::{self, Color32, FontId, RichText, TextFormat};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

const MAX_READING_WIDTH: f32 = 920.0;
const COMPACT_GUTTER: f32 = 16.0;
const WIDE_GUTTER: f32 = 32.0;

pub fn show(ui: &mut egui::Ui, markdown: &str) {
    let available_width = ui.available_width();
    let (side_gutter, content_width) = reading_column(available_width);

    ui.add_space(18.0);
    ui.horizontal(|ui| {
        // This row is only a positioning device, so its normal inter-widget
        // spacing must not inflate the calculated gutters.
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.add_space(side_gutter);
        ui.vertical(|ui| {
            ui.set_width(content_width);
            render(ui, markdown);
        });
    });
    ui.add_space(28.0);
}

fn reading_column(available_width: f32) -> (f32, f32) {
    let preferred_gutter = if available_width >= 600.0 {
        WIDE_GUTTER
    } else {
        COMPACT_GUTTER
    };
    let minimum_gutter = preferred_gutter.min(available_width * 0.1);
    let content_width = (available_width - minimum_gutter * 2.0).clamp(0.0, MAX_READING_WIDTH);
    ((available_width - content_width) * 0.5, content_width)
}

fn render(ui: &mut egui::Ui, markdown: &str) {
    let visuals = ui.visuals().clone();
    let mut job = egui::text::LayoutJob::default();
    let mut size = 15.0;
    let mut strong = false;
    let mut italic = false;
    let mut code_block = false;
    let mut code = String::new();
    let mut list_depth = 0usize;
    let mut link = None;
    for event in Parser::new_ext(
        markdown,
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS,
    ) {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                flush(ui, &mut job);
                size = match level {
                    HeadingLevel::H1 => 28.0,
                    HeadingLevel::H2 => 23.0,
                    HeadingLevel::H3 => 19.0,
                    _ => 17.0,
                };
                strong = true;
            }
            Event::End(TagEnd::Heading(_)) => {
                flush(ui, &mut job);
                size = 15.0;
                strong = false;
                ui.add_space(6.0);
            }
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => {
                flush(ui, &mut job);
                ui.add_space(4.0);
            }
            Event::Start(Tag::Strong) => strong = true,
            Event::End(TagEnd::Strong) => strong = false,
            Event::Start(Tag::Emphasis) => italic = true,
            Event::End(TagEnd::Emphasis) => italic = false,
            Event::Start(Tag::List(_)) => list_depth += 1,
            Event::End(TagEnd::List(_)) => {
                list_depth = list_depth.saturating_sub(1);
                ui.add_space(4.0);
            }
            Event::Start(Tag::Item) => {
                flush(ui, &mut job);
                append(
                    &mut job,
                    &format!("{}•  ", "  ".repeat(list_depth.saturating_sub(1))),
                    size,
                    false,
                    false,
                    false,
                    &visuals,
                );
            }
            Event::End(TagEnd::Item) => flush(ui, &mut job),
            Event::Start(Tag::CodeBlock(_)) => {
                flush(ui, &mut job);
                code_block = true;
                code.clear();
            }
            Event::End(TagEnd::CodeBlock) => {
                let available_width = ui.available_width();
                egui::Frame::group(ui.style())
                    .fill(visuals.code_bg_color)
                    .inner_margin(egui::Margin::symmetric(12, 10))
                    .show(ui, |ui| {
                        ui.set_min_width((available_width - 24.0).max(0.0));
                        ui.add(
                            egui::Label::new(
                                RichText::new(code.trim_end())
                                    .monospace()
                                    .size(13.0)
                                    .color(visuals.strong_text_color()),
                            )
                            .selectable(true),
                        );
                    });
                code_block = false;
                ui.add_space(6.0);
            }
            Event::Start(Tag::Link { dest_url, .. }) => link = Some(dest_url.into_string()),
            Event::End(TagEnd::Link) => {
                flush(ui, &mut job);
                if let Some(url) = link.take() {
                    if url.starts_with("https://") || url.starts_with("http://") {
                        ui.hyperlink(&url);
                    } else {
                        ui.label(RichText::new(url).small().color(visuals.text_color()));
                    }
                }
            }
            Event::Text(text) if code_block => code.push_str(&text),
            Event::Text(text) => append(&mut job, &text, size, strong, italic, false, &visuals),
            Event::Code(text) => append(&mut job, &text, size - 1.0, false, false, true, &visuals),
            Event::SoftBreak => append(&mut job, " ", size, strong, italic, false, &visuals),
            Event::HardBreak => append(&mut job, "\n", size, strong, italic, false, &visuals),
            Event::Rule => {
                flush(ui, &mut job);
                ui.separator();
            }
            Event::TaskListMarker(checked) => append(
                &mut job,
                if checked { "[x] " } else { "[ ] " },
                size,
                false,
                false,
                true,
                &visuals,
            ),
            Event::Html(text) | Event::InlineHtml(text) => {
                append(&mut job, &text, size - 1.0, false, false, true, &visuals)
            }
            _ => {}
        }
    }
    flush(ui, &mut job);
}

fn append(
    job: &mut egui::text::LayoutJob,
    text: &str,
    size: f32,
    strong: bool,
    italic: bool,
    code: bool,
    visuals: &egui::Visuals,
) {
    let format = TextFormat {
        font_id: if code {
            FontId::monospace(size)
        } else {
            FontId::proportional(size)
        },
        color: if strong || code {
            visuals.strong_text_color()
        } else {
            visuals.text_color()
        },
        italics: italic,
        background: if code {
            visuals.code_bg_color
        } else {
            Color32::TRANSPARENT
        },
        ..Default::default()
    };
    job.append(text, 0.0, format);
}

fn flush(ui: &mut egui::Ui, job: &mut egui::text::LayoutJob) {
    if !job.text.is_empty() {
        job.wrap.max_width = ui.available_width();
        ui.add(egui::Label::new(std::mem::take(job)).selectable(true));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luminance(color: Color32) -> f32 {
        let linear = |value: u8| {
            let value = f32::from(value) / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(color.r()) + 0.7152 * linear(color.g()) + 0.0722 * linear(color.b())
    }

    fn check_text_shapes(shape: &egui::Shape, visuals: &egui::Visuals, seen: &mut String) {
        match shape {
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    check_text_shapes(shape, visuals, seen);
                }
            }
            egui::Shape::Text(text) => {
                let job = &text.galley.job;
                for section in &job.sections {
                    let content = &job.text[section.byte_range.start.0..section.byte_range.end.0];
                    if content.trim().is_empty() {
                        continue;
                    }
                    let background = if section.format.background.a() != 0 {
                        section.format.background
                    } else if content.contains("fenced_code") {
                        visuals.code_bg_color
                    } else {
                        visuals.panel_fill
                    };
                    let foreground = luminance(section.format.color);
                    let background = luminance(background);
                    let contrast =
                        (foreground.max(background) + 0.05) / (foreground.min(background) + 0.05);
                    assert!(
                        contrast >= 4.5,
                        "Unreadable text {content:?}: contrast {contrast}, dark={}",
                        visuals.dark_mode
                    );
                    seen.push_str(content);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn rendered_markdown_stays_readable_on_light_and_dark_backgrounds() {
        for theme in [egui::Theme::Light, egui::Theme::Dark] {
            let ctx = egui::Context::default();
            ctx.set_theme(theme);
            let output = ctx.run_ui(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 600.0))),
                ..Default::default()
            }, |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    show(ui, "# Heading\n\nParagraph with **emphasis** and `inline_code`.\n\n- List item\n\n```python\nfenced_code()\n```\n");
                });
            });
            let visuals = ctx.style_of(theme).visuals.clone();
            let mut seen = String::new();
            for clipped in &output.shapes {
                check_text_shapes(&clipped.shape, &visuals, &mut seen);
            }
            for expected in [
                "Heading",
                "Paragraph",
                "emphasis",
                "inline_code",
                "List item",
                "fenced_code",
            ] {
                assert!(seen.contains(expected), "Missing rendered text: {expected}");
            }
            output.drop_without_applying_deltas();
        }
    }

    #[test]
    fn reading_column_keeps_responsive_gutters_and_limits_line_length() {
        assert_eq!(reading_column(500.0), (16.0, 468.0));
        assert_eq!(reading_column(1_080.0), (80.0, MAX_READING_WIDTH));
        assert_eq!(reading_column(100.0), (10.0, 80.0));
    }
}

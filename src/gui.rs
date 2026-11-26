use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use eframe::egui;

enum Message {
    Validation(u64, String, Vec<TagCheck>, Option<u64>, Option<String>),
    Suggestions(SuggestionField, String, Result<Vec<crate::api::TagSuggestion>, String>),
    Log(String),
    Finished(FinishStatus),
}

#[derive(Clone, Copy)]
enum FinishStatus {
    Completed,
    Cancelled,
    Failed,
}

#[derive(Clone)]
struct TagCheck {
    tag: String,
    excluded: bool,
    valid: bool,
    count: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SuggestionField {
    Include,
    Exclude,
}

#[derive(Default)]
struct SuggestionState {
    query: String,
    values: Vec<crate::api::TagSuggestion>,
    loading: bool,
}

pub fn run() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Rule34 Fetcher")
            .with_inner_size([760.0, 740.0])
            .with_min_inner_size([600.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Rule34 Fetcher",
        options,
        Box::new(|_| Ok(Box::<FetcherApp>::default())),
    )
}

struct FetcherApp {
    user_id: String,
    api_key: String,
    api_settings_open: bool,
    saved_user_id: String,
    saved_api_key: String,
    show_user_id: bool,
    show_api_key: bool,
    credentials_status: String,
    include_tags: String,
    exclude_tags: String,
    post_limit: usize,
    from_page: u32,
    to_page: u32,
    available_pages: Option<u32>,
    check_key: Option<String>,
    checks: Vec<TagCheck>,
    total_count: Option<u64>,
    count_error: Option<String>,
    checking: bool,
    validation_generation: u64,
    running: bool,
    stopping: bool,
    cancellation: Option<Arc<AtomicBool>>,
    include_suggestions: SuggestionState,
    exclude_suggestions: SuggestionState,
    enabled_tag_categories: [bool; 5],
    suggestion_generations: [Arc<AtomicU64>; 2],
    messages: Vec<String>,
    rx: Receiver<Message>,
    tx: Sender<Message>,
}

impl Default for FetcherApp {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        let (user_id, api_key) = crate::saved_api_credentials().unwrap_or_default();
        Self {
            saved_user_id: user_id.clone(),
            saved_api_key: api_key.clone(),
            user_id,
            api_key,
            api_settings_open: false,
            show_user_id: false,
            show_api_key: false,
            credentials_status: String::new(),
            include_tags: String::new(),
            exclude_tags: String::new(),
            post_limit: 100,
            from_page: 1,
            to_page: 1,
            available_pages: None,
            check_key: None,
            checks: Vec::new(),
            total_count: None,
            count_error: None,
            checking: false,
            validation_generation: 0,
            running: false,
            stopping: false,
            cancellation: None,
            include_suggestions: SuggestionState::default(),
            exclude_suggestions: SuggestionState::default(),
            enabled_tag_categories: [true; 5],
            suggestion_generations: [
                Arc::new(AtomicU64::new(0)),
                Arc::new(AtomicU64::new(0)),
            ],
            messages: vec![
                "Укажите теги и проверьте их. Данные API читаются из r34.env.".into(),
            ],
            rx,
            tx,
        }
    }
}

impl FetcherApp {
    fn credentials_saved(&self) -> bool {
        !self.saved_user_id.is_empty()
            && !self.saved_api_key.is_empty()
            && self.user_id.trim() == self.saved_user_id
            && self.api_key.trim() == self.saved_api_key
    }

    fn save_credentials(&mut self) {
        match crate::save_api_credentials(&self.user_id, &self.api_key) {
            Ok(()) => {
                self.saved_user_id = self.user_id.trim().to_owned();
                self.saved_api_key = self.api_key.trim().to_owned();
                self.credentials_status = "Учётные данные сохранены в r34.env.".into();
                if !self.tokens().is_empty() {
                    self.start_check();
                }
            }
            Err(error) => self.credentials_status = format!("Не удалось сохранить: {error:#}"),
        }
    }

    fn current_key(&self) -> String {
        format!("{}\n-{}", self.include_tags.trim(), self.exclude_tags.trim())
    }

    fn tokens(&self) -> Vec<String> {
        let mut tokens = split_tags(&self.include_tags);
        tokens.extend(
            split_tags(&self.exclude_tags)
                .into_iter()
                .map(|tag| format!("-{tag}")),
        );
        tokens
    }

    fn tags_changed(&mut self) {
        self.validation_generation = self.validation_generation.wrapping_add(1);
        self.checking = false;
        self.check_key = None;
        self.checks.clear();
        self.total_count = None;
        self.count_error = None;
        self.available_pages = None;
    }

    fn start_check(&mut self) {
        let key = self.current_key();
        let tags = self.tokens();
        if tags.is_empty() {
            self.messages.push("Введите хотя бы один тег.".into());
            return;
        }
        self.checking = true;
        self.validation_generation = self.validation_generation.wrapping_add(1);
        let generation = self.validation_generation;
        self.check_key = Some(key.clone());
        self.checks.clear();
        self.total_count = None;
        self.count_error = None;
        let tx = self.tx.clone();
        thread::spawn(move || {
            let outcome = check_tags(tags);
            match outcome {
                Ok((checks, total_count, count_error)) => {
                    let _ = tx.send(Message::Validation(
                        generation,
                        key,
                        checks,
                        total_count,
                        count_error,
                    ));
                }
                Err(error) => {
                    let _ = tx.send(Message::Log(format!("Ошибка проверки тегов: {error}")));
                    let _ = tx.send(Message::Validation(
                        generation,
                        key,
                        Vec::new(),
                        None,
                        None,
                    ));
                }
            }
        });
    }

    fn start_download(&mut self) {
        let tags = self.tokens();
        let tx = self.tx.clone();
        let limit = self.post_limit;
        let from_page = self.from_page;
        let to_page = self.to_page;
        let cancellation = Arc::new(AtomicBool::new(false));
        let worker_cancellation = cancellation.clone();
        self.cancellation = Some(cancellation);
        self.running = true;
        self.stopping = false;
        self.messages.push(format!(
            "Запуск: страницы {}–{}, максимум {} постов.",
            from_page, to_page, limit
        ));
        thread::spawn(move || {
            run_worker(tags, limit, from_page, to_page, worker_cancellation, tx)
        });
    }

    fn receive_messages(&mut self) {
        while let Ok(message) = self.rx.try_recv() {
            match message {
                Message::Validation(generation, key, checks, total_count, count_error) => {
                    if generation == self.validation_generation {
                        self.checking = false;
                    }
                    if generation == self.validation_generation
                        && self.check_key.as_deref() == Some(&key)
                    {
                        self.checks = checks;
                        self.total_count = total_count;
                        self.count_error = count_error;
                        let valid = self.checks.iter().filter(|check| check.valid).count();
                        self.messages.push(format!(
                            "Проверка завершена: существующих тегов {valid}/{}.",
                            self.checks.len()
                        ));
                        if let Some(count) = self.total_count {
                            let pages = count.div_ceil(100).min(200) as u32;
                            self.available_pages = Some(pages);
                            self.to_page = pages.max(1);
                            self.from_page = self.from_page.min(self.to_page).max(1);
                            self.messages.push(format!(
                                "Всего постов по включениям и исключениям: {}.",
                                format_count(count)
                            ));
                        } else if let Some(error) = &self.count_error {
                            self.messages.push(format!("Не удалось получить общий счётчик: {error}"));
                        }
                    }
                }
                Message::Suggestions(field, query, result) => {
                    let state = self.suggestions_mut(field);
                    if state.query == query {
                        state.loading = false;
                        state.values = result.unwrap_or_default();
                    }
                }
                Message::Log(line) => self.messages.push(line),
                Message::Finished(status) => {
                    self.running = false;
                    self.stopping = false;
                    self.cancellation = None;
                    self.messages.push(match status {
                        FinishStatus::Completed => "Загрузка завершена.".into(),
                        FinishStatus::Cancelled => "Загрузка остановлена.".into(),
                        FinishStatus::Failed => "Задача завершилась с ошибкой.".into(),
                    });
                }
            }
        }
        if self.messages.len() > 500 {
            self.messages.drain(..100);
        }
    }

    fn tags_are_valid(&self) -> bool {
        !self.checks.is_empty()
            && self.check_key.as_deref() == Some(&self.current_key())
            && self.checks.iter().all(|check| check.valid)
    }

    fn suggestions_mut(&mut self, field: SuggestionField) -> &mut SuggestionState {
        match field {
            SuggestionField::Include => &mut self.include_suggestions,
            SuggestionField::Exclude => &mut self.exclude_suggestions,
        }
    }

    fn request_suggestions(&mut self, field: SuggestionField, input: &str) {
        let generation_index = match field {
            SuggestionField::Include => 0,
            SuggestionField::Exclude => 1,
        };
        let generation = self.suggestion_generations[generation_index]
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        let generation_state = Arc::clone(&self.suggestion_generations[generation_index]);
        let query = last_tag(input).trim_start_matches('-').to_owned();
        let state = self.suggestions_mut(field);
        state.query = query.clone();
        state.values.clear();
        if query.chars().count() < 2 {
            state.loading = false;
            return;
        }
        state.loading = true;
        let tx = self.tx.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(300));
            if generation_state.load(Ordering::Relaxed) != generation {
                return;
            }
            let result = fetch_suggestions(&query).map_err(|error| error.to_string());
            let _ = tx.send(Message::Suggestions(field, query, result));
        });
    }

    fn show_suggestions(
        &mut self,
        ctx: &egui::Context,
        field: SuggestionField,
        anchor: egui::Rect,
        focused: bool,
    ) {
        let enabled_categories = self.enabled_tag_categories;
        let show_unknown = enabled_categories.iter().all(|enabled| *enabled);
        let (query, values) = {
            let state = self.suggestions_mut(field);
            (state.query.clone(), state.values.clone())
        };
        let suggestions = values
            .iter()
            .filter(|suggestion| match suggestion.category {
                Some(category) => crate::api::TagCategory::ALL
                    .iter()
                    .position(|candidate| *candidate == category)
                    .is_some_and(|index| enabled_categories[index]),
                None => show_unknown,
            })
            .cloned()
            .collect::<Vec<_>>();
        if suggestions.is_empty() {
            return;
        }
        let normalized_query = query.trim().to_ascii_lowercase().replace(' ', "_");
        let (mut normal_suggestions, mut reverse_suggestions): (Vec<_>, Vec<_>) = suggestions
            .into_iter()
            .partition(|suggestion| {
                suggestion
                    .tag
                    .trim()
                    .to_ascii_lowercase()
                    .replace(' ', "_")
                    .starts_with(&normalized_query)
            });
        normal_suggestions.truncate(100);
        reverse_suggestions.truncate(100);
        let popup_size = egui::vec2(anchor.width(), 238.0);
        let popup_rect = egui::Rect::from_min_size(
            egui::pos2(anchor.left(), anchor.bottom() + 2.0),
            popup_size,
        );
        let pointer_over_popup = ctx
            .pointer_hover_pos()
            .is_some_and(|pointer| popup_rect.contains(pointer));
        if !focused && !pointer_over_popup {
            return;
        }
        let mut selected = None;
        let popup_id = match field {
            SuggestionField::Include => "include-tag-suggestions",
            SuggestionField::Exclude => "exclude-tag-suggestions",
        };
        egui::Area::new(egui::Id::new(popup_id))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(anchor.left(), anchor.bottom() + 2.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(anchor.width());
                    ui.columns(2, |columns| {
                        let groups = [
                            ("Обычные", normal_suggestions, "normal"),
                            ("Обратные", reverse_suggestions, "reverse"),
                        ];
                        for (column_index, (heading, suggestions, suffix)) in
                            groups.into_iter().enumerate()
                        {
                            columns[column_index].heading(heading);
                            egui::ScrollArea::vertical()
                                .id_salt(format!("{popup_id}-{suffix}"))
                                .max_height(200.0)
                                .show(&mut columns[column_index], |ui| {
                                    for suggestion in suggestions {
                                        let label = suggestion
                                            .count
                                            .map(|count| {
                                                format!(
                                                    "{} ({})",
                                                    suggestion.tag,
                                                    format_count(count)
                                                )
                                            })
                                            .unwrap_or_else(|| suggestion.tag.clone());
                                        if ui.selectable_label(false, label).clicked() {
                                            selected = Some(suggestion.tag);
                                        }
                                    }
                                });
                        }
                    });
                });
            });
        if let Some(tag) = selected {
            let input = match field {
                SuggestionField::Include => &mut self.include_tags,
                SuggestionField::Exclude => &mut self.exclude_tags,
            };
            replace_last_tag(input, &tag);
            let input = input.clone();
            self.tags_changed();
            self.request_suggestions(field, &input);
            self.start_check();
        }
    }
}

impl eframe::App for FetcherApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.receive_messages();
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Rule34 Fetcher");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    egui::ComboBox::from_id_salt("tag-category-filter")
                        .selected_text("Категории тегов")
                        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                        .show_ui(ui, |ui| {
                            for (index, category) in crate::api::TagCategory::ALL.iter().enumerate() {
                                ui.checkbox(
                                    &mut self.enabled_tag_categories[index],
                                    category.label(),
                                );
                            }
                        });
                });
            });
            ui.label("Поиск и загрузка оригиналов через API сайта");
            ui.add_space(12.0);

            ui.label("Теги для поиска (через пробел):");
            let include_response = ui.add(
                egui::TextEdit::singleline(&mut self.include_tags)
                    .hint_text("например: 1girl solo")
                    .id(egui::Id::new("include-tags-input"))
                    .desired_width(f32::INFINITY),
            );
            if include_response.changed() {
                let input = self.include_tags.clone();
                self.tags_changed();
                self.request_suggestions(SuggestionField::Include, &input);
                if input_ends_tag(&input) {
                    self.start_check();
                }
            }
            if include_response.has_focus() {
                self.exclude_suggestions.values.clear();
            }
            self.show_suggestions(
                ctx,
                SuggestionField::Include,
                include_response.rect,
                include_response.has_focus(),
            );
            ui.add_space(8.0);
            ui.label("Исключить теги (через пробел):");
            let exclude_response = ui.add(
                egui::TextEdit::singleline(&mut self.exclude_tags)
                    .hint_text("например: watermark lowres")
                    .id(egui::Id::new("exclude-tags-input"))
                    .desired_width(f32::INFINITY),
            );
            if exclude_response.changed() {
                let input = self.exclude_tags.clone();
                self.tags_changed();
                self.request_suggestions(SuggestionField::Exclude, &input);
                if input_ends_tag(&input) {
                    self.start_check();
                }
            }
            if exclude_response.has_focus() {
                self.include_suggestions.values.clear();
            }
            self.show_suggestions(
                ctx,
                SuggestionField::Exclude,
                exclude_response.rect,
                exclude_response.has_focus(),
            );
            ui.label("Исключения будут переданы сайту с префиксом «-», например -watermark.");

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.label("Постов максимум:");
                ui.add(egui::DragValue::new(&mut self.post_limit).range(1..=20_000));
            });
            ui.horizontal(|ui| {
                ui.label("Страницы с:");
                ui.add(egui::DragValue::new(&mut self.from_page).range(1..=200));
                ui.label("по:");
                ui.add(egui::DragValue::new(&mut self.to_page).range(1..=200));
                ui.label("(включительно, по 100 постов на страницу)");
                if let Some(pages) = self.available_pages {
                    ui.label(format!("доступно: {pages}"));
                }
            });
            ui.add_space(10.0);

            let changed = self.check_key.as_deref() != Some(&self.current_key());
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!self.checking && !self.running, egui::Button::new("Проверить теги"))
                    .clicked()
                {
                    self.start_check();
                }
                let can_download = self.credentials_saved()
                    && self.tags_are_valid()
                    && !self.running
                    && !self.checking
                    && self.from_page <= self.to_page;
                if ui
                    .add_enabled(can_download, egui::Button::new("Найти и скачать"))
                    .clicked()
                {
                    self.start_download();
                }
                if ui
                    .add_enabled(
                        self.running && !self.stopping,
                        egui::Button::new("Остановить загрузку"),
                    )
                    .clicked()
                {
                    if let Some(cancellation) = &self.cancellation {
                        cancellation.store(true, Ordering::Relaxed);
                    }
                    self.stopping = true;
                    self.messages.push("Отправлена команда остановки…".into());
                }
                if self.checking {
                    ui.spinner();
                    ui.label("Проверяю теги…");
                } else if self.stopping {
                    ui.spinner();
                    ui.label("Останавливаю загрузку…");
                } else if changed && self.check_key.is_some() {
                    ui.label("Теги изменились — проверьте их заново.");
                }
            });

            if !self.checks.is_empty() && !changed {
                ui.horizontal_wrapped(|ui| {
                    for check in &self.checks {
                        let color = if check.valid {
                            egui::Color32::from_rgb(50, 150, 75)
                        } else {
                            egui::Color32::from_rgb(190, 55, 55)
                        };
                        let tag_label = if check.excluded {
                            format!("-{}", check.tag)
                        } else {
                            check.tag.clone()
                        };
                        let count = check
                            .count
                            .map(|count| format!(" ({})", format_count(count)))
                            .unwrap_or_default();
                        ui.horizontal(|ui| {
                            paint_tag_status_icon(ui, check.valid, color);
                            ui.colored_label(color, format!("{tag_label}{count}"));
                        });
                    }
                });
                ui.label("Число в скобках — количество постов с этим тегом.");
                if let Some(total_count) = self.total_count {
                    ui.strong(format!(
                        "Совпадений после исключений: {}",
                        format_count(total_count)
                    ));
                }
            }

            ui.add_space(10.0);
            ui.group(|ui| {
                ui.set_max_width(445.0);
                let row_width = ui.available_width().min(445.0);
                let action_width = 24.0;
                let item_gap = ui.spacing().item_spacing.x;
                ui.horizontal(|ui| {
                    let (title_rect, title) = ui.allocate_exact_size(
                        egui::vec2(row_width - action_width - item_gap, 24.0),
                        egui::Sense::click(),
                    );
                    ui.painter().text(
                        title_rect.left_center() + egui::vec2(4.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        "Данные API (хранятся в r34.env)",
                        egui::FontId::proportional(14.0),
                        ui.visuals().text_color(),
                    );
                    if title.clicked() {
                        self.api_settings_open = !self.api_settings_open;
                    }
                    let arrow = ui
                        .add_sized([action_width, 24.0], egui::Button::new(""))
                        .on_hover_text("Открыть/закрыть настройки API");
                    if arrow.clicked() {
                        self.api_settings_open = !self.api_settings_open;
                    }
                    let rect = arrow.rect;
                    let center = rect.center();
                    let half_width = 4.0;
                    let half_height = 2.5;
                    let stroke = egui::Stroke::new(1.6_f32, ui.visuals().text_color());
                    let (left, tip, right) = if self.api_settings_open {
                        (
                            egui::pos2(center.x - half_width, center.y + half_height),
                            egui::pos2(center.x, center.y - half_height),
                            egui::pos2(center.x + half_width, center.y + half_height),
                        )
                    } else {
                        (
                            egui::pos2(center.x - half_width, center.y - half_height),
                            egui::pos2(center.x, center.y + half_height),
                            egui::pos2(center.x + half_width, center.y - half_height),
                        )
                    };
                    ui.painter().line_segment([left, tip], stroke);
                    ui.painter().line_segment([tip, right], stroke);
                });
                if self.api_settings_open {
                    let label_width = 54.0;
                    let field_width =
                        (row_width - label_width - action_width - item_gap * 2.0).max(120.0);
                    ui.horizontal(|ui| {
                        ui.add_sized([label_width, 24.0], egui::Label::new("user_id"));
                        ui.add_sized(
                            [field_width, 24.0],
                            egui::TextEdit::singleline(&mut self.user_id)
                                .password(!self.show_user_id),
                        );
                        let button = if self.show_user_id { "🙈" } else { "👁" };
                        if ui
                            .add_sized([action_width, 24.0], egui::Button::new(button))
                            .on_hover_text("Показать/скрыть user_id")
                            .clicked()
                        {
                            self.show_user_id = !self.show_user_id;
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.add_sized([label_width, 24.0], egui::Label::new("api_key"));
                        ui.add_sized(
                            [field_width, 24.0],
                            egui::TextEdit::singleline(&mut self.api_key)
                                .password(!self.show_api_key),
                        );
                        let button = if self.show_api_key { "🙈" } else { "👁" };
                        if ui
                            .add_sized([action_width, 24.0], egui::Button::new(button))
                            .on_hover_text("Показать/скрыть api_key")
                            .clicked()
                        {
                            self.show_api_key = !self.show_api_key;
                        }
                    });
                    ui.horizontal(|ui| {
                        if ui.button("Сохранить в r34.env").clicked() {
                            self.save_credentials();
                        }
                        if !self.credentials_status.is_empty() {
                            ui.label(&self.credentials_status);
                        }
                    });
                    if !self.credentials_saved() {
                        ui.colored_label(
                            egui::Color32::from_rgb(190, 110, 35),
                            "Изменения не сохранены; перед поиском нажмите кнопку сохранения.",
                        );
                    }
                }
            });

            ui.add_space(10.0);
            ui.separator();
            ui.label("Журнал");
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .max_height(300.0)
                .show(ui, |ui| {
                    for line in &self.messages {
                        ui.label(line);
                    }
                });
        });
        if self.running
            || self.checking
            || self.include_suggestions.loading
            || self.exclude_suggestions.loading
        {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
}

fn split_tags(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || c == ',')
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .map(|tag| tag.trim_start_matches('-').to_string())
        .filter(|tag| !tag.is_empty())
        .collect()
}

fn last_tag(text: &str) -> &str {
    if text
        .chars()
        .last()
        .is_some_and(|character| character.is_whitespace() || character == ',')
    {
        return "";
    }
    text.rsplit(|character: char| character.is_whitespace() || character == ',')
        .next()
        .unwrap_or("")
}

fn input_ends_tag(text: &str) -> bool {
    text.chars()
        .last()
        .is_some_and(|character| character.is_whitespace() || character == ',')
        && !split_tags(text).is_empty()
}

fn replace_last_tag(text: &mut String, replacement: &str) {
    let start = text
        .char_indices()
        .rev()
        .find(|(_, character)| character.is_whitespace() || *character == ',')
        .map(|(index, character)| index + character.len_utf8())
        .unwrap_or(0);
    text.truncate(start);
    text.push_str(replacement);
    text.push(' ');
}

fn fetch_suggestions(query: &str) -> anyhow::Result<Vec<crate::api::TagSuggestion>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let client = reqwest::Client::builder()
        .user_agent(crate::util::USER_AGENT)
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(15))
        .build()?;
    let credentials = crate::saved_api_credentials().ok();
    let credential_refs = credentials
        .as_ref()
        .filter(|(user_id, api_key)| !user_id.trim().is_empty() && !api_key.trim().is_empty())
        .map(|(user_id, api_key)| (user_id.as_str(), api_key.as_str()));
    runtime.block_on(crate::api::tag_suggestions(&client, query, credential_refs, true))
}

fn check_tags(
    tags: Vec<String>,
) -> anyhow::Result<(Vec<TagCheck>, Option<u64>, Option<String>)> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let client = reqwest::Client::builder()
        .user_agent(crate::util::USER_AGENT)
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(30))
        .build()?;
    let credentials = crate::api_credentials_for_gui().ok();
    runtime.block_on(async move {
        let mut output = Vec::new();
        for search_token in &tags {
            let excluded = search_token.starts_with('-');
            let tag = search_token.trim_start_matches('-');
            if is_meta_tag(tag) {
                output.push(TagCheck {
                    tag: tag.to_owned(),
                    excluded,
                    valid: true,
                    count: None,
                });
                continue;
            }
            let normalized = tag.trim().to_ascii_lowercase().replace(' ', "_");
            let credential_refs = credentials
                .as_ref()
                .map(|(user_id, api_key)| (user_id.as_str(), api_key.as_str()));
            let match_tag = crate::api::tag_suggestions(&client, tag, credential_refs, false)
                .await?
                .into_iter()
                .find(|suggestion| {
                    suggestion.tag.trim().to_ascii_lowercase().replace(' ', "_") == normalized
                });
            output.push(TagCheck {
                tag: tag.to_owned(),
                excluded,
                valid: match_tag.is_some(),
                count: match_tag.and_then(|suggestion| suggestion.count),
            });
            tokio::time::sleep(Duration::from_millis(120)).await;
        }
        let mut total_count = None;
        let mut count_error = None;
        if output.iter().all(|check| check.valid) {
            match credentials {
                Some((user_id, api_key)) => {
                    match crate::api::search_count(
                        &client,
                        &user_id,
                        &api_key,
                        &tags.join(" "),
                    )
                    .await
                    {
                        Ok(count) => total_count = Some(count),
                        Err(error) => count_error = Some(error.to_string()),
                    }
                }
                None => count_error = Some("проверьте user_id и api_key в r34.env".into()),
            }
        }
        Ok((output, total_count, count_error))
    })
}

fn format_count(count: u64) -> String {
    let digits = count.to_string();
    let first_group = digits.len() % 3;
    let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);
    if first_group > 0 {
        formatted.push_str(&digits[..first_group]);
    }
    for chunk in digits[first_group..].as_bytes().chunks(3) {
        if !formatted.is_empty() {
            formatted.push(' ');
        }
        formatted.push_str(std::str::from_utf8(chunk).unwrap_or(""));
    }
    formatted
}

fn paint_tag_status_icon(ui: &mut egui::Ui, valid: bool, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
    let stroke = egui::Stroke::new(2.0_f32, color);
    if valid {
        ui.painter().line_segment(
            [
                rect.left_top() + egui::vec2(2.0, 7.0),
                rect.left_top() + egui::vec2(5.0, 10.0),
            ],
            stroke,
        );
        ui.painter().line_segment(
            [
                rect.left_top() + egui::vec2(5.0, 10.0),
                rect.left_top() + egui::vec2(12.0, 3.0),
            ],
            stroke,
        );
    } else {
        ui.painter().line_segment(
            [
                rect.left_top() + egui::vec2(3.0, 3.0),
                rect.left_top() + egui::vec2(11.0, 11.0),
            ],
            stroke,
        );
        ui.painter().line_segment(
            [
                rect.left_top() + egui::vec2(11.0, 3.0),
                rect.left_top() + egui::vec2(3.0, 11.0),
            ],
            stroke,
        );
    }
}

fn is_meta_tag(tag: &str) -> bool {
    ["order:", "id:", "rating:", "score:", "date:", "user:"]
        .iter()
        .any(|prefix| tag.to_ascii_lowercase().starts_with(prefix))
}

fn run_worker(
    tags: Vec<String>,
    limit: usize,
    from_page: u32,
    to_page: u32,
    cancellation: Arc<AtomicBool>,
    tx: Sender<Message>,
) {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            let _ = tx.send(Message::Log(format!("Не удалось найти exe: {error}")));
            let _ = tx.send(Message::Finished(FinishStatus::Failed));
            return;
        }
    };
    let mut command = Command::new(exe);
    command
        .current_dir(crate::util::project_dir())
        .env_remove("R34_USER_ID")
        .env_remove("R34_API_KEY")
        .arg("--worker-cli")
        .arg("--limit")
        .arg(limit.to_string())
        .arg("--from-page")
        .arg(from_page.to_string())
        .arg("--to-page")
        .arg(to_page.to_string())
        .arg("--page-size")
        .arg("100")
        .arg("--")
        .args(&tags)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let _ = tx.send(Message::Log(format!("Не удалось запустить поиск: {error}")));
            let _ = tx.send(Message::Finished(FinishStatus::Failed));
            return;
        }
    };
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    if let Some(stdout) = stdout {
        forward_lines(stdout, tx.clone());
    }
    if let Some(stderr) = stderr {
        forward_lines(stderr, tx.clone());
    }
    loop {
        if cancellation.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            cleanup_partial_files(&tags);
            let _ = tx.send(Message::Finished(FinishStatus::Cancelled));
            return;
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let _ = tx.send(Message::Finished(if status.success() {
                    FinishStatus::Completed
                } else {
                    FinishStatus::Failed
                }));
                return;
            }
            Ok(None) => thread::sleep(Duration::from_millis(100)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                cleanup_partial_files(&tags);
                let _ = tx.send(Message::Log(format!("Ошибка ожидания загрузчика: {error}")));
                let _ = tx.send(Message::Finished(FinishStatus::Failed));
                return;
            }
        }
    }
}

fn cleanup_partial_files(tags: &[String]) {
    let folder = crate::util::project_dir()
        .join("downloads")
        .join(crate::util::folder_name(&tags.join(" ")));
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".part"))
        {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn forward_lines<R: Read + Send + 'static>(stream: R, tx: Sender<Message>) {
    thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            let _ = tx.send(Message::Log(line));
        }
    });
}

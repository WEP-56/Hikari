use super::*;
use gpui_kit::component::attachment::{
    Attachment as AttachmentCard, AttachmentActions, AttachmentContent, AttachmentGroup,
    AttachmentMedia, AttachmentTitle,
};
use gpui_kit::Axis;
use hikari_agent::{Attachment, AttachmentStore};
use std::path::{Path, PathBuf};

impl Preview {
    pub(super) fn current_attachments(&self) -> &[Attachment] {
        self.attachment_drafts
            .get(&self.active.map(|i| self.talks[i].id))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
    fn attachment_store(&self, id: Option<u64>) -> AttachmentStore {
        AttachmentStore::new(
            self.session_store
                .path()
                .parent()
                .unwrap()
                .join("artifacts")
                .join(id.map_or_else(|| "draft".into(), |id| format!("talk-{id}"))),
        )
    }
    fn add_attachment(&mut self, attachment: Attachment, cx: &mut Context<Self>) {
        let key = self.active.map(|i| self.talks[i].id);
        let draft = self.attachment_drafts.entry(key).or_default();
        if draft.len() >= 8 {
            self.note_composer("每条消息最多 8 张图片", cx);
            return;
        }
        if !draft.iter().any(|a| a.id == attachment.id) {
            draft.push(attachment);
        }
        self.dismiss_composer_notice(cx);
    }
    pub(super) fn dismiss_composer_notice(&mut self, cx: &mut Context<Self>) {
        if self.workspace_notice.is_empty() {
            return;
        }
        self.workspace_notice.clear();
        self.notice_generation = self.notice_generation.wrapping_add(1);
        cx.notify();
    }
    fn note_composer(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.workspace_notice = message.into();
        self.notice_generation = self.notice_generation.wrapping_add(1);
        let generation = self.notice_generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_secs(4))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.notice_generation == generation {
                    this.workspace_notice.clear();
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }
    pub(super) fn attach_paths(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        let store = self.attachment_store(self.active.map(|i| self.talks[i].id));
        for path in paths.iter().take(8) {
            match store.import_file(path) {
                Ok(attachment) => self.add_attachment(attachment, cx),
                Err(error) => self.note_composer(error, cx),
            }
        }
    }
    pub(super) fn paste_images(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(item) = cx.read_from_clipboard() else {
            return false;
        };
        let mut handled = false;
        for entry in item.entries() {
            match entry {
                ClipboardEntry::Image(image) => {
                    handled = true;
                    let store = self.attachment_store(self.active.map(|i| self.talks[i].id));
                    match import_clipboard_image(&store, image) {
                        Ok(attachment) => self.add_attachment(attachment, cx),
                        Err(error) => self.note_composer(error, cx),
                    }
                }
                ClipboardEntry::ExternalPaths(paths) => {
                    handled = true;
                    self.attach_paths(&paths.0, cx);
                }
                _ => {}
            }
        }
        handled
    }
    pub(super) fn choose_images(&mut self, cx: &mut Context<Self>) {
        let key = self.active.map(|i| self.talks[i].id);
        let result = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("选择图片".into()),
        });
        cx.spawn(async move |this, cx| {
            let paths = result.await;
            let _ = this.update(cx, |this, cx| {
                if this.active.map(|i| this.talks[i].id) != key {
                    return;
                }
                if let Ok(Ok(Some(paths))) = paths {
                    this.attach_paths(&paths, cx);
                }
            });
        })
        .detach();
    }
    pub(super) fn save_input_attachments(
        &self,
        id: u64,
        attachments: &[Attachment],
    ) -> Result<Vec<Attachment>, String> {
        let store = self.attachment_store(Some(id));
        attachments
            .iter()
            .map(|a| {
                // Verify the resource before moving the reference into a persistent session.
                a.data_url()?;
                store.import(
                    &a.name,
                    &a.mime_type,
                    &std::fs::read(&a.path).map_err(|e| e.to_string())?,
                )
            })
            .collect()
    }
    pub(super) fn attachment_draft_view(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let key = self.active.map(|i| self.talks[i].id);
        let view = cx.entity().downgrade();
        AttachmentGroup::new("draft-attachments")
            .w_full()
            .flex_shrink_0()
            .children(self.current_attachments().iter().enumerate().map(
                |(index, attachment)| {
                    draft_card(attachment, index, key, view.clone())
                },
            ))
    }
}

pub(super) fn attachment_strip(
    items: &[(String, Attachment)],
    group_id: impl Into<ElementId>,
    cx: &mut Context<Preview>,
) -> AnyElement {
    if items.is_empty() {
        return div().into_any_element();
    }
    let view = cx.entity().downgrade();
    AttachmentGroup::new(group_id)
        .w_full()
        .min_w_0()
        .flex_shrink_0()
        .children(
            items
                .iter()
                .map(|(id, attachment)| message_card(id, attachment, view.clone())),
        )
        .into_any_element()
}

fn message_card(id: &str, attachment: &Attachment, view: WeakEntity<Preview>) -> AnyElement {
    let open = attachment.clone();
    let media = if attachment.is_image() {
        AttachmentMedia::new().src(attachment.path.clone())
    } else {
        AttachmentMedia::new().child(Icon::new(IconName::File))
    };
    let mut card = AttachmentCard::new()
        .id(SharedString::from(id.to_string()))
        .axis(Axis::Vertical)
        .large()
        .w(px(220.))
        .media(media)
        .content(
            AttachmentContent::new().title(AttachmentTitle::new(attachment.name.clone())),
        )
        .on_click(move |_, _, cx| {
            if open.is_image() {
                open_preview(open.clone(), cx);
            } else if let Some(parent) = open.path.parent() {
                cx.reveal_path(parent);
            }
        });
    if attachment.is_image() {
        card = card.actions(
            AttachmentActions::new().child(save_button(
                SharedString::from(format!("{id}-save")),
                attachment.clone(),
                view,
            )),
        );
    }
    card.into_any_element()
}

fn draft_card(
    attachment: &Attachment,
    index: usize,
    key: Option<u64>,
    view: WeakEntity<Preview>,
) -> AnyElement {
    let open = attachment.clone();
    let id = format!("draft-image-{index}");
    let card = AttachmentCard::new()
        .id(SharedString::from(id.clone()))
        .small()
        .media(if attachment.is_image() {
            AttachmentMedia::new().src(attachment.path.clone())
        } else {
            AttachmentMedia::new().child(Icon::new(IconName::File))
        })
        .content(AttachmentContent::new().title(AttachmentTitle::new(attachment.name.clone())))
        .on_click(move |_, _, cx| {
            if open.is_image() {
                open_preview(open.clone(), cx);
            }
        })
        .on_remove(move |_, _, cx| {
            let _ = view.update(cx, |this, cx| {
                if let Some(draft) = this.attachment_drafts.get_mut(&key) {
                    if index < draft.len() {
                        draft.remove(index);
                    }
                }
                cx.notify();
            });
        });
    let tracked = div().relative().flex_shrink_0().child(card);
    #[cfg(test)]
    let tracked = tracked
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, _, _, _| {
                    crate::chat::tests::record_bounds(id.clone(), bounds);
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full(),
        )
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, _, _, _| {
                    crate::chat::tests::record_bounds(format!("remove-image-{index}"), bounds);
                },
            )
            .absolute()
            .top_0()
            .right_0()
            .size(px(20.)),
        );
    tracked.into_any_element()
}

fn save_button(
    id: SharedString,
    attachment: Attachment,
    view: WeakEntity<Preview>,
) -> impl IntoElement {
    Button::new(id)
        .ghost()
        .xsmall()
        .label("另存")
        .tooltip("把图片保存到选择的位置")
        .on_click(move |_, _, cx| begin_save(attachment.clone(), view.clone(), cx))
}

fn begin_save(attachment: Attachment, view: WeakEntity<Preview>, cx: &mut App) {
    let receiver = cx.prompt_for_new_path(&save_directory(), Some(&attachment.name));
    cx.spawn(async move |cx| {
        let chosen = receiver.await;
        let _ = view.update(cx, |this, cx| {
            match chosen {
                Ok(Ok(Some(path))) => match copy_attachment(&attachment, &path) {
                    Ok(()) => this.note_composer(format!("已另存：{}", path.display()), cx),
                    Err(error) => this.note_composer(format!("另存失败：{error}"), cx),
                },
                Ok(Err(error)) => {
                    this.note_composer(format!("无法打开保存窗口：{error}"), cx);
                }
                _ => {}
            }
            cx.notify();
        });
    })
    .detach();
}

fn open_preview(attachment: Attachment, cx: &mut App) {
    let bounds = Bounds::centered(None, size(px(960.), px(720.)), cx);
    let result = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..Default::default()
        },
        move |_, cx| {
            cx.new(|_| ImagePreview {
                attachment,
                notice: String::new(),
            })
        },
    );
    if let Err(error) = result {
        eprintln!("Failed to open image preview: {error}");
    }
}

fn import_clipboard_image(
    store: &AttachmentStore,
    image: &gpui_kit::Image,
) -> Result<Attachment, String> {
    use gpui_kit::ImageFormat;
    match image.format {
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Webp | ImageFormat::Gif => store
            .import(
                &format!("粘贴图片.{}", image.format.extension()),
                image.format.mime_type(),
                &image.bytes,
            )
            .or_else(|_| {
                // Win+V sometimes labels a bitmap as a browser image type.
                let png = raster_to_png(&image.bytes)?;
                store.import("粘贴图片.png", "image/png", &png)
            }),
        ImageFormat::Svg => Err("不能把 SVG 当作图片附件".into()),
        ImageFormat::Bmp | ImageFormat::Tiff | ImageFormat::Ico | ImageFormat::Pnm => {
            // Clipboard history (Win+V) usually offers screenshots as CF_DIB/BMP.
            let png = raster_to_png(&image.bytes)?;
            store.import("粘贴图片.png", "image/png", &png)
        }
    }
}

fn raster_to_png(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let image = image::load_from_memory(bytes)
        .map_err(|_| "不支持的图片格式或文件内容不匹配".to_string())?;
    let mut png = Vec::new();
    image
        .write_to(
            &mut std::io::Cursor::new(&mut png),
            image::ImageFormat::Png,
        )
        .map_err(|error| format!("无法把剪贴板图片转成 PNG：{error}"))?;
    Ok(png)
}

fn save_directory() -> PathBuf {
    if let Some(home) = std::env::var_os("USERPROFILE") {
        let downloads = PathBuf::from(&home).join("Downloads");
        if downloads.is_dir() {
            return downloads;
        }
        let home = PathBuf::from(home);
        if home.is_dir() {
            return home;
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn copy_attachment(attachment: &Attachment, path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty() {
        return Err("没有选择保存位置".into());
    }
    if path == attachment.path {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    std::fs::copy(&attachment.path, path).map_err(|e| e.to_string())?;
    Ok(())
}

struct ImagePreview {
    attachment: Attachment,
    notice: String,
}

impl ImagePreview {
    fn save(&mut self, cx: &mut Context<Self>) {
        let attachment = self.attachment.clone();
        let receiver = cx.prompt_for_new_path(&save_directory(), Some(&attachment.name));
        cx.spawn(async move |this, cx| {
            let chosen = receiver.await;
            let _ = this.update(cx, |this, cx| {
                this.notice = match chosen {
                    Ok(Ok(Some(path))) => match copy_attachment(&this.attachment, &path) {
                        Ok(()) => format!("已另存：{}", path.display()),
                        Err(error) => format!("另存失败：{error}"),
                    },
                    Ok(Err(error)) => format!("无法打开保存窗口：{error}"),
                    _ => this.notice.clone(),
                };
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for ImagePreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let path = self.attachment.path.clone();
        let notice = self.notice.clone();
        v_flex().size_full().bg(rgb(0x16171a)).text_color(rgb(0xf4f4f5)).child(
            h_flex()
                .flex_shrink_0()
                .items_center()
                .justify_between()
                .gap_2()
                .px_3()
                .py_2()
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .text_sm()
                        .child(self.attachment.name.clone()),
                )
                .child(
                    h_flex()
                        .flex_shrink_0()
                        .gap_2()
                        .child(
                            Button::new("reveal-image")
                                .ghost()
                                .small()
                                .label("打开位置")
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    if let Some(parent) = path.parent() {
                                        cx.reveal_path(parent);
                                    }
                                })),
                        )
                        .child(
                            Button::new("save-image")
                                .small()
                                .label("另存")
                                .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                        ),
                ),
        )
        .child(
            div()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .p_4()
                .child(
                    img(self.attachment.path.clone())
                        .size_full()
                        .object_fit(ObjectFit::Contain),
                ),
        )
        .when(!notice.is_empty(), |this| {
            this.child(
                div()
                    .flex_shrink_0()
                    .px_3()
                    .pb_2()
                    .text_xs()
                    .child(notice),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn clipboard_bmp_from_win_v_is_stored_as_png() {
        let mut bitmap = image::RgbImage::new(2, 2);
        bitmap.put_pixel(0, 0, image::Rgb([12, 34, 56]));
        let mut bmp = Vec::new();
        image::DynamicImage::ImageRgb8(bitmap)
            .write_to(
                &mut std::io::Cursor::new(&mut bmp),
                image::ImageFormat::Bmp,
            )
            .unwrap();
        assert!(bmp.starts_with(b"BM"));
        let png = super::raster_to_png(&bmp).unwrap();
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        let root = std::env::temp_dir().join(format!("hikari-clip-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let store = hikari_agent::AttachmentStore::new(root.clone());
        let saved = store.import("粘贴图片.png", "image/png", &png).unwrap();
        assert_eq!(saved.mime_type, "image/png");
        assert!(saved.is_image());
        std::fs::remove_dir_all(root).ok();
    }
}

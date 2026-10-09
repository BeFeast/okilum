//! Responsive block-image layout, keeping intrinsic pixels out of document height.
use gpui::*;

pub(crate) struct ReaderImage {
    source: ImageSource,
    error_message: Option<SharedString>,
    cache: Option<Entity<RetainAllImageCache>>,
}
impl ReaderImage {
    pub(crate) fn new(source: ImageSource) -> Self {
        Self {
            source,
            error_message: None,
            cache: None,
        }
    }

    pub(crate) fn with_error_message(mut self, message: &'static str) -> Self {
        self.error_message = Some(message.into());
        self
    }

    pub(crate) fn with_cache(mut self, cache: Entity<RetainAllImageCache>) -> Self {
        self.cache = Some(cache);
        self
    }

    fn image(&self) -> Img {
        let mut image = img(self.source.clone());
        if let Some(message) = self.error_message.clone() {
            image = image.with_fallback(move || {
                div()
                    .debug_selector(|| "reader-image-unreadable".into())
                    .child(message.clone())
                    .into_any_element()
            });
        }
        image
    }
}

fn fitted_size(intrinsic: Size<Pixels>, available: Pixels) -> Size<Pixels> {
    if intrinsic.width <= px(0.) || intrinsic.height <= px(0.) {
        return size(px(0.), px(0.));
    }
    let width = available.max(px(0.)).min(intrinsic.width);
    size(width, width * (intrinsic.height / intrinsic.width))
}

impl IntoElement for ReaderImage {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for ReaderImage {
    type RequestLayoutState = ();
    type PrepaintState = AnyElement;
    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        // Reuse GPUI's asynchronous loader/cache to discover intrinsic geometry.
        // Until it is available, reserve no fictitious full-size image height.
        let intrinsic = window.with_image_cache(self.cache.clone().map(Into::into), |window| {
            self.image().into_any_element().layout_as_root(
                size(AvailableSpace::MaxContent, AvailableSpace::MaxContent),
                window,
                cx,
            )
        });
        let mut style = Style::default();
        style.max_size.width = intrinsic.width.into();
        let id = window.request_measured_layout(style, move |known, available, _, _| {
            let width = known
                .width
                .or(match available.width {
                    AvailableSpace::Definite(width) => Some(width),
                    _ => None,
                })
                .unwrap_or(intrinsic.width);
            fitted_size(intrinsic, width)
        });
        (id, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let mut image = self
            .image()
            .id("reader-block-image")
            .debug_selector(|| "reader-block-image".into())
            .w(bounds.size.width)
            .h(bounds.size.height)
            .object_fit(ObjectFit::Contain)
            .into_any_element();
        window.with_image_cache(self.cache.clone().map(Into::into), |window| {
            image.prepaint_as_root(
                bounds.origin,
                size(
                    AvailableSpace::Definite(bounds.size.width),
                    AvailableSpace::Definite(bounds.size.height),
                ),
                window,
                cx,
            )
        });
        image
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        image: &mut AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.with_image_cache(self.cache.clone().map(Into::into), |window| {
            image.paint(window, cx)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    #[test]
    fn image_height_tracks_actual_width_before_and_after_loading() {
        assert_eq!(fitted_size(size(px(0.), px(0.)), px(640.)).height, px(0.));
        let intrinsic = size(px(2600.), px(862.));
        for width in [640_f32, 320., 100., 2600., 3000.] {
            let fitted = fitted_size(intrinsic, px(width));
            assert_eq!(fitted.width, px(width.min(2600.)));
            assert!((f32::from(fitted.height) - width.min(2600.) * 862. / 2600.).abs() < 0.01);
        }
    }
    #[gpui::test]
    fn loaded_image_block_and_following_text_resize_together(cx: &mut TestAppContext) {
        struct ImageView {
            source: ImageSource,
            width: f32,
        }
        impl Render for ImageView {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                gpui_component::v_flex()
                    .w(px(self.width))
                    .child(
                        div()
                            .id("image-box")
                            .debug_selector(|| "image-box".into())
                            .child(ReaderImage::new(self.source.clone())),
                    )
                    .child(
                        div()
                            .id("after-image")
                            .debug_selector(|| "after-image".into())
                            .h(px(20.))
                            .child("Following text"),
                    )
            }
        }
        cx.update(gpui_component::init);
        let image = ImageSource::Image(std::sync::Arc::new(Image::from_bytes(ImageFormat::Svg,
            br##"<svg xmlns="http://www.w3.org/2000/svg" width="2600" height="862"><rect width="2600" height="862" fill="#336699"/></svg>"##.to_vec())));
        let (view, visual) = cx.add_window_view(|_, _| ImageView {
            source: image,
            width: 640.,
        });
        visual.update(|window, cx| window.draw(cx).clear(cx));
        visual.run_until_parked();
        for width in [640., 320., 740., 640.] {
            view.update(visual, |v, cx| {
                v.width = width;
                cx.notify();
            });
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
            let bounds = visual.debug_bounds("image-box").unwrap();
            let after = visual.debug_bounds("after-image").unwrap();
            let expected = px(width * 862. / 2600.);
            assert!(bounds.size.height > px(1.), "loaded-image positive control");
            assert!(
                (bounds.size.height - expected).abs() < px(1.),
                "{bounds:?}, width={width}"
            );
            assert!(
                (after.top() - bounds.bottom()).abs() < px(1.),
                "no intrinsic-height blank space"
            );
        }
    }
}

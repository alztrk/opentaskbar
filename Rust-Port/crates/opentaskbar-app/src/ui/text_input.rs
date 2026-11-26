//! Small single-line text input built on GPUI's platform input handler.
//!
//! GPUI 0.2 does not ship a ready-made `Input` element. This wrapper delegates layout and
//! painting to the wrapped element (usually a stateful `Div`), then registers the owning
//! entity as the active IME/text handler for the frame. Character input, IME composition
//! and paste are handled by the platform; the entity supplies the buffer via
//! `EntityInputHandler`.

use gpui::{
    App, Bounds, Element, ElementId, ElementInputHandler, Entity, EntityInputHandler, FocusHandle,
    GlobalElementId, InspectorElementId, IntoElement, LayoutId, Pixels, Window,
};

pub(crate) struct TextInput<T, D>
where
    T: EntityInputHandler,
    D: Element,
{
    inner: D,
    focus: FocusHandle,
    view: Entity<T>,
}

impl<T, D> TextInput<T, D>
where
    T: EntityInputHandler,
    D: Element,
{
    pub(crate) fn new(inner: D, focus: FocusHandle, view: Entity<T>) -> Self {
        Self { inner, focus, view }
    }
}

impl<T, D> IntoElement for TextInput<T, D>
where
    T: EntityInputHandler,
    D: Element,
{
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl<T, D> Element for TextInput<T, D>
where
    T: EntityInputHandler,
    D: Element,
{
    type RequestLayoutState = D::RequestLayoutState;
    type PrepaintState = D::PrepaintState;

    fn id(&self) -> Option<ElementId> {
        self.inner.id()
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        self.inner.source_location()
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        self.inner.request_layout(id, inspector_id, window, cx)
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.inner
            .prepaint(id, inspector_id, bounds, request_layout, window, cx)
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.inner.paint(
            id,
            inspector_id,
            bounds,
            request_layout,
            prepaint,
            window,
            cx,
        );
        window.handle_input(
            &self.focus,
            ElementInputHandler::new(bounds, self.view.clone()),
            cx,
        );
    }
}

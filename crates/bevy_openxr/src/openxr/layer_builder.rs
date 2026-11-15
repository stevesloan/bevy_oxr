use std::mem;

use bevy_ecs::world::World;
use bevy_mod_xr::spaces::{XrPrimaryReferenceSpace, XrSpace};
use openxr::{sys, CompositionLayerFlags, Fovf, Posef, Rect2Di};

use crate::graphics::graphics_match;
use crate::resources::*;
use crate::spaces::OxrSpaceExt as _;

pub trait LayerProvider {
    fn get<'a>(&'a self, world: &'a World) -> Option<Box<dyn CompositionLayer<'a> + 'a>>;
}

pub struct ProjectionLayer;

pub struct PassthroughLayer;

impl LayerProvider for ProjectionLayer {
    fn get<'a>(&self, world: &'a World) -> Option<Box<dyn CompositionLayer<'a> + 'a>> {
        let stage = world.get_resource::<XrPrimaryReferenceSpace>()?;
        let openxr_views = world.get_resource::<OxrViews>()?;
        let swapchain = world.get_resource::<OxrSwapchain>()?;
        let graphics_info = world.get_resource::<OxrCurrentSessionConfig>()?;
        let rect = openxr::Rect2Di {
            offset: openxr::Offset2Di { x: 0, y: 0 },
            extent: openxr::Extent2Di {
                width: graphics_info.resolution.x as _,
                height: graphics_info.resolution.y as _,
            },
        };

        if openxr_views.len() < 2 {
            return None;
        }

        Some(Box::new(
            CompositionLayerProjection::new()
                .layer_flags(CompositionLayerFlags::BLEND_TEXTURE_SOURCE_ALPHA)
                .space(stage)
                .views(&[
                    CompositionLayerProjectionView::new()
                        .pose(openxr_views.0[0].pose)
                        .fov(openxr_views.0[0].fov)
                        .sub_image(
                            SwapchainSubImage::new()
                                .swapchain(swapchain)
                                .image_array_index(0)
                                .image_rect(rect),
                        ),
                    CompositionLayerProjectionView::new()
                        .pose(openxr_views.0[1].pose)
                        .fov(openxr_views.0[1].fov)
                        .sub_image(
                            SwapchainSubImage::new()
                                .swapchain(swapchain)
                                .image_array_index(1)
                                .image_rect(rect),
                        ),
                ]),
        ))
    }
}

impl LayerProvider for PassthroughLayer {
    fn get(&self, world: &World) -> Option<Box<dyn CompositionLayer<'_>>> {
        Some(Box::new(
            CompositionLayerPassthroughFB::new()
                .layer_handle(world.get_resource::<OxrPassthroughLayerFB>()?)
                .layer_flags(CompositionLayerFlags::BLEND_TEXTURE_SOURCE_ALPHA),
        ))
    }
}

#[derive(Copy, Clone)]
pub struct SwapchainSubImage<'a> {
    inner: sys::SwapchainSubImage,
    swapchain: Option<&'a OxrSwapchain>,
}

impl<'a> SwapchainSubImage<'a> {
    #[inline]
    pub fn new() -> Self {
        Self {
            inner: sys::SwapchainSubImage {
                ..unsafe { mem::zeroed() }
            },
            swapchain: None,
        }
    }
    #[inline]
    pub fn into_raw(self) -> sys::SwapchainSubImage {
        self.inner
    }
    #[inline]
    pub fn as_raw(&self) -> &sys::SwapchainSubImage {
        &self.inner
    }
    #[inline]
    pub fn swapchain(mut self, value: &'a OxrSwapchain) -> Self {
        graphics_match!(
            &value.0;
            swap => self.inner.swapchain = swap.as_raw()
        );
        self.swapchain = Some(value);
        self
    }
    #[inline]
    pub fn image_rect(mut self, value: Rect2Di) -> Self {
        self.inner.image_rect = value;
        self
    }
    #[inline]
    pub fn image_array_index(mut self, value: u32) -> Self {
        self.inner.image_array_index = value;
        self
    }
}

impl Default for SwapchainSubImage<'_> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Copy, Clone)]
pub struct CompositionLayerProjectionView<'a> {
    inner: sys::CompositionLayerProjectionView,
    swapchain: Option<&'a OxrSwapchain>,
}

impl<'a> CompositionLayerProjectionView<'a> {
    #[inline]
    pub fn new() -> Self {
        Self {
            inner: sys::CompositionLayerProjectionView {
                ty: sys::StructureType::COMPOSITION_LAYER_PROJECTION_VIEW,
                ..unsafe { mem::zeroed() }
            },
            swapchain: None,
        }
    }
    #[inline]
    pub fn into_raw(self) -> sys::CompositionLayerProjectionView {
        self.inner
    }
    #[inline]
    pub fn as_raw(&self) -> &sys::CompositionLayerProjectionView {
        &self.inner
    }
    #[inline]
    pub fn pose(mut self, value: Posef) -> Self {
        self.inner.pose = value;
        self
    }
    #[inline]
    pub fn fov(mut self, value: Fovf) -> Self {
        self.inner.fov = value;
        self
    }
    #[inline]
    pub fn sub_image(mut self, value: SwapchainSubImage<'a>) -> Self {
        self.inner.sub_image = value.inner;
        self.swapchain = value.swapchain;
        self
    }
}
impl Default for CompositionLayerProjectionView<'_> {
    fn default() -> Self {
        Self::new()
    }
}
/// # Safety
/// the header function must return a ref to a valid Composition Layer struct.
/// it has to use `repr(C)` and it has to follow the shape of a Composition Layer struct from the
/// OpenXR specification
pub unsafe trait CompositionLayer<'a> {
    fn swapchain(&self) -> Option<&'a OxrSwapchain>;
    fn header(&self) -> &sys::CompositionLayerBaseHeader;
    /// A copy of this layer that borrows nothing, so `xrEndFrame` can run on
    /// another thread after the world it was built from has moved on.
    ///
    /// `None` keeps the frame's `xrEndFrame` on the render thread, which is
    /// always correct. That is the default, and what a layer must answer if
    /// its struct points at anything it can't carry along.
    fn detach(&self) -> Option<Box<dyn DetachedLayer>> {
        None
    }
}

/// A composition layer that owns everything its struct points to, apart from
/// OpenXR handles.
///
/// # Safety
/// As for [`CompositionLayer`]. In addition, every pointer reachable from the
/// header must stay valid for as long as this value lives, wherever it is moved.
pub unsafe trait DetachedLayer: Send {
    fn header(&self) -> &sys::CompositionLayerBaseHeader;
}

struct DetachedProjection {
    inner: sys::CompositionLayerProjection,
    // `inner.views` points into this allocation, which moving the struct
    // leaves where it is.
    _views: Vec<sys::CompositionLayerProjectionView>,
}

// SAFETY: the raw pointers are to the struct's own heap allocation; the rest
// are OpenXR handles, which the runtime accepts from any thread.
unsafe impl Send for DetachedProjection {}

unsafe impl DetachedLayer for DetachedProjection {
    fn header(&self) -> &sys::CompositionLayerBaseHeader {
        unsafe { mem::transmute(&self.inner) }
    }
}

struct DetachedPassthroughFB(sys::CompositionLayerPassthroughFB);

// SAFETY: holds no pointer but a null `next`.
unsafe impl Send for DetachedPassthroughFB {}

unsafe impl DetachedLayer for DetachedPassthroughFB {
    fn header(&self) -> &sys::CompositionLayerBaseHeader {
        unsafe { mem::transmute(&self.0) }
    }
}
#[derive(Clone)]
pub struct CompositionLayerProjection<'a> {
    inner: sys::CompositionLayerProjection,
    swapchain: Option<&'a OxrSwapchain>,
    views: Vec<sys::CompositionLayerProjectionView>,
}
impl<'a> CompositionLayerProjection<'a> {
    #[inline]
    pub fn new() -> Self {
        Self {
            inner: sys::CompositionLayerProjection {
                ty: sys::StructureType::COMPOSITION_LAYER_PROJECTION,
                ..unsafe { mem::zeroed() }
            },
            swapchain: None,
            views: Vec::new(),
        }
    }
    #[inline]
    pub fn into_raw(self) -> sys::CompositionLayerProjection {
        self.inner
    }
    #[inline]
    pub fn as_raw(&self) -> &sys::CompositionLayerProjection {
        &self.inner
    }
    #[inline]
    pub fn layer_flags(mut self, value: CompositionLayerFlags) -> Self {
        self.inner.layer_flags = value;
        self
    }
    #[inline]
    pub fn space(mut self, value: &XrSpace) -> Self {
        self.inner.space = value.as_raw_openxr_space();
        self
    }
    #[inline]
    pub fn views(mut self, value: &[CompositionLayerProjectionView<'a>]) -> Self {
        self.views = value.iter().map(|view| view.inner).collect();
        self.inner.views = self.views.as_slice().as_ptr() as *const _ as _;
        self.inner.view_count = self.views.len() as u32;
        self
    }
}
unsafe impl<'a> CompositionLayer<'a> for CompositionLayerProjection<'a> {
    fn swapchain(&self) -> Option<&'a OxrSwapchain> {
        self.swapchain
    }

    fn header(&self) -> &sys::CompositionLayerBaseHeader {
        unsafe { mem::transmute(&self.inner) }
    }

    fn detach(&self) -> Option<Box<dyn DetachedLayer>> {
        // An extension chain is someone else's memory.
        if !self.inner.next.is_null() || self.views.iter().any(|view| !view.next.is_null()) {
            return None;
        }
        let views = self.views.clone();
        let mut inner = self.inner;
        inner.views = views.as_ptr();
        inner.view_count = views.len() as u32;
        Some(Box::new(DetachedProjection {
            inner,
            _views: views,
        }))
    }
}
impl Default for CompositionLayerProjection<'_> {
    fn default() -> Self {
        Self::new()
    }
}
pub struct CompositionLayerPassthroughFB {
    inner: sys::CompositionLayerPassthroughFB,
}
impl Default for CompositionLayerPassthroughFB {
    fn default() -> Self {
        Self::new()
    }
}

impl CompositionLayerPassthroughFB {
    #[inline]
    pub const fn new() -> Self {
        Self {
            inner: openxr::sys::CompositionLayerPassthroughFB {
                ty: openxr::sys::CompositionLayerPassthroughFB::TYPE,
                ..unsafe { mem::zeroed() }
            },
        }
    }
    #[inline]
    pub fn layer_handle(mut self, layer_handle: &OxrPassthroughLayerFB) -> Self {
        self.inner.layer_handle = *layer_handle.inner();
        self
    }
    #[inline]
    pub fn layer_flags(mut self, value: CompositionLayerFlags) -> Self {
        self.inner.flags = value;
        self
    }
}
unsafe impl<'a> CompositionLayer<'a> for CompositionLayerPassthroughFB {
    fn swapchain(&self) -> Option<&'a OxrSwapchain> {
        None
    }

    fn header(&self) -> &sys::CompositionLayerBaseHeader {
        unsafe { mem::transmute(&self.inner) }
    }

    fn detach(&self) -> Option<Box<dyn DetachedLayer>> {
        self.inner
            .next
            .is_null()
            .then(|| Box::new(DetachedPassthroughFB(self.inner)) as Box<dyn DetachedLayer>)
    }
}

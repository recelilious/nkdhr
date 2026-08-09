//! Shared two-dimensional primitive renderer for nkdhr.
//!
//! Applications record validated, renderer-independent [`DisplayList`] values.
//! The production [`gles::GlesBackend`] draws them inside Smithay's active GLES
//! frame, while [`software::SoftwareRenderer`] provides a deterministic test
//! oracle for golden images.

mod display_list;
mod geometry;
pub mod gles;
pub mod software;
mod texture;

pub use display_list::{
    BackdropBlurPrimitive, BuildError, DisplayList, DisplayListBuilder, Primitive, Shadow,
    ShapePrimitive, ShapeStyle, TexturePrimitive,
};
pub use geometry::{Color, CornerRadii, Point, Rect, Transform};
pub use texture::{
    AlphaMode, Sampling, TextureAsset, TextureError, TextureFormat, TextureId, TextureStore,
};

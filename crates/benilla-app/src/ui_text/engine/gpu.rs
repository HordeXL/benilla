//! The glyph cache in Bevy: building it, following the window, and uploading each new cell as a
//! sub-rect `RenderQueue::write_texture` into the sheet's texture, never through `Assets<Image>`
//! ([`crate::ui_text::pack`] says why).

use std::sync::{Arc, Mutex};

use bevy::image::Image;
use bevy::prelude::*;
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::{
    Extent3d, Origin3d, TexelCopyBufferLayout, TexelCopyTextureInfo, TextureAspect,
};
use bevy::render::renderer::RenderQueue;
use bevy::render::texture::GpuImage;
use bevy::render::{ExtractSchedule, Render, RenderApp, RenderSystems};
use bevy::text::CosmicFontSystem;
use bevy::window::PrimaryWindow;

use benilla_assets::{LockRecover, WorldAssets};

use super::{faces, TextEngine, UiFontAtlas};
use crate::ui_text::pack::CellUpload;

/// Cells rasterized this frame, waiting for the render world.
#[derive(Resource, Default)]
struct GlyphUploadQueue(Vec<CellUpload>);

/// The render world's copy, which also keeps cells whose texture was not ready yet.
#[derive(Resource, Default)]
struct GlyphUploads(Vec<CellUpload>);

/// Loads the client's TTFs and drives the on-demand glyph cache.
pub(crate) struct UiTextPlugin;

/// The `Startup` build of the glyph cache, for a VM built at `Startup` to order after.
#[derive(SystemSet, Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct FontAtlasInit;

impl Plugin for UiTextPlugin {
    fn build(&self, app: &mut App) {
        // `init` retries each Update until the patch chain and the window's real `scale_factor`
        // exist. `publish_sheet` runs in `Last`, after every producer of text quads, so a cell
        // rasterized this frame is queued before the render extract.
        // It runs at `Startup` too, so the stock glue's first screen, built then, measures its text
        // inline as the reference's does: `AccountLogin_ShowUserAgreements` sizes the agreement
        // header off `TOSFrameTitle:GetWidth()` in that show.
        app.init_resource::<GlyphUploadQueue>()
            .add_systems(
                Startup,
                init.in_set(FontAtlasInit)
                    .after(benilla_assets::AssetSet::Open),
            )
            .add_systems(Update, (init, load_cjk_face_into_bevy_text))
            .add_systems(Last, publish_sheet);
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app
            .init_resource::<GlyphUploads>()
            .add_systems(ExtractSchedule, extract_glyph_uploads)
            .add_systems(
                Render,
                // After `prepare_assets::<GpuImage>`, so a sheet made this frame has its texture.
                upload_glyph_cells.in_set(RenderSystems::PrepareResources),
            );
    }
}

/// Build the engine on the first frame with both the patch chain and the window's `scale_factor`.
fn init(
    mut commands: Commands,
    world_assets: Option<Res<WorldAssets>>,
    images: Res<Assets<Image>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    existing: Option<Res<UiFontAtlas>>,
) {
    if existing.is_some() {
        return;
    }
    let (Some(world_assets), Ok(window)) = (world_assets, windows.single()) else {
        return;
    };
    let Some(engine) = TextEngine::load(&world_assets, &images, window.scale_factor()) else {
        return;
    };
    commands.insert_resource(UiFontAtlas {
        engine: Arc::new(Mutex::new(engine)),
        generation: 0,
        ellipsis: crate::ui_text::EllipsisMemo::default(),
    });
}

/// Marks [`load_cjk_face_into_bevy_text`] done, so the face is loaded into Bevy's font system once.
#[derive(Resource)]
struct CjkFaceInBevyText;

/// Put the install's own CJK face into *Bevy's* font system, which the glue screens draw through.
///
/// There are two font systems here. [`TextEngine`] shapes the in-game UI off its own database, and
/// [`TextEngine::load`] already registers the install's CJK face there. Bevy's `Text` — the login,
/// realm, character-select and character-create screens — shapes off `CosmicFontSystem` instead,
/// whose database holds only the faces loaded as Bevy `Font` assets: `Fonts\FRIZQT__.TTF` and
/// `Fonts\ARIALN.TTF`, neither of which carries a single CJK code point. A localized client's glue
/// strings are Chinese (`GlueStrings.lua`, 429 entries), so every one of them drew as `.notdef`
/// boxes until this ran.
///
/// Loading the face is enough: cosmic-text's fallback reaches any face in the database, asserted by
/// `differential_tests::a_whole_string_falls_back_to_the_install_cjk_face`. Latin keeps Friz and
/// only the characters Friz cannot draw land on the CJK face.
///
/// It must run *before* the first shape, not merely before the first frame that shows text:
/// cosmic-text caches a shape run by `(text, attrs)` with no font-database version in the key, so a
/// string shaped while the face is missing keeps its `.notdef` answer after the face lands. `Update`
/// precedes the `PostUpdate` shaping systems, and `WorldAssets` is inserted in `Startup`, so the
/// face is in the database first. Moving this later would fail silently, back to boxes.
///
/// The reference picks the glue face from the client's `GlueXML\GlueFonts.xml` (`GlueFontNormal`,
/// `Fonts\FZLBJW.TTF` on this zhCN install); benilla takes the install's best CJK face instead, the
/// same discovery [`TextEngine`] uses, so a publisher renaming the face cannot break it. The Latin
/// face stays `FRIZQT__`, the enUS glue font benilla already hardcodes.
fn load_cjk_face_into_bevy_text(
    mut commands: Commands,
    world_assets: Option<Res<WorldAssets>>,
    mut font_system: Option<ResMut<CosmicFontSystem>>,
    done: Option<Res<CjkFaceInBevyText>>,
) {
    if done.is_some() {
        return;
    }
    let (Some(world_assets), Some(font_system)) = (world_assets, font_system.as_mut()) else {
        return;
    };
    let chain = world_assets.chain.lock_recover();
    // `Chain::list`, the union of every archive's listfile: one archive's own answers with
    // `speech2.MPQ`'s sound-only list, which holds no `.ttf` at all.
    let listed: Vec<String> = chain
        .list()
        .map(|entries| entries.into_iter().map(|e| e.name).collect())
        .unwrap_or_else(|e| {
            warn!("bevy text: the patch chain has no listfile union to scan for a CJK face: {e:#}");
            Vec::new()
        });
    // Dropped before the read: `read_font` locks the chain itself, and the `Mutex` is not
    // reentrant. The loose `Fonts\` folder wins there too, so a replacement face is what lands.
    drop(chain);
    let Some((path, bytes)) = faces::cjk_fallback_face(&listed, |n| world_assets.read_font(n))
    else {
        // A Latin-only install: nothing to add, and Bevy's own faces already cover its text.
        return;
    };
    font_system
        .0
        .db_mut()
        .load_font_source(fontdb::Source::Binary(
            Arc::new(bytes) as Arc<dyn AsRef<[u8]> + Sync + Send>
        ));
    info!("bevy text: the install's CJK face {path} is loaded for the glue screens' fallback");
    commands.insert_resource(CjkFaceInBevyText);
}

/// The frame boundary: follow the window's DPI, carry out a pending reset, create the sheet's
/// texture once, and hand this frame's cells to the render world. A reset waits for this boundary
/// so no UV moves while quads that use it are still being pushed.
fn publish_sheet(
    atlas: Option<ResMut<UiFontAtlas>>,
    mut images: ResMut<Assets<Image>>,
    mut queue: ResMut<GlyphUploadQueue>,
    windows: Query<&Window, With<PrimaryWindow>>,
) {
    let Some(mut atlas) = atlas else {
        return;
    };
    let dpi = windows.single().map_or(1.0, Window::scale_factor);
    let (generation, dpi_moved) = {
        let mut e = atlas.lock();
        let dpi_moved = (e.dpi - dpi).abs() > 1e-6;
        e.dpi = dpi;
        if e.reset_pending {
            e.reset_pending = false;
            e.generation += 1;
            e.stats.resets += 1;
            e.chars.clear();
            e.cells.clear();
            e.sheet.reset();
        }
        let (announce, mut cells) = e.sheet.take_pending();
        if announce {
            // Inserted once on the reserved handle, never `get_mut` after: that would recreate
            // the texture and blank every glyph. A failed insert has no recovery.
            let _ = images.insert(e.sheet.handle().id(), crate::ui_text::pack::sheet_image());
        }
        queue.0.append(&mut cells);
        (e.generation, dpi_moved)
    };
    // A DPI change does stale the ellipsis memo: its answers fit a box at the old raster size.
    if dpi_moved {
        atlas.ellipsis = crate::ui_text::EllipsisMemo::default();
    }
    atlas.generation = generation;
    report_cache(&atlas);
}

/// Hand the frame's cells to the render world, appending to any a previous frame could not write.
fn extract_glyph_uploads(
    mut main_world: ResMut<bevy::render::MainWorld>,
    mut uploads: ResMut<GlyphUploads>,
) {
    if let Some(mut q) = main_world.get_resource_mut::<GlyphUploadQueue>() {
        uploads.0.append(&mut q.0);
    }
}

/// Write each new cell into the sheet's texture as a sub-rect, so the texture's identity never
/// changes; a cell whose texture is not prepared yet stays queued.
fn upload_glyph_cells(
    mut uploads: ResMut<GlyphUploads>,
    gpu_images: Res<RenderAssets<GpuImage>>,
    render_queue: Res<RenderQueue>,
) {
    uploads.0.retain(|u| {
        let Some(gpu) = gpu_images.get(u.image) else {
            return true; // the texture is not up yet: retry next frame
        };
        render_queue.write_texture(
            TexelCopyTextureInfo {
                texture: &gpu.texture,
                mip_level: 0,
                origin: Origin3d {
                    x: u.x,
                    y: u.y,
                    z: 0,
                },
                aspect: TextureAspect::All,
            },
            &u.rgba,
            TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(u.w * 4),
                rows_per_image: Some(u.h),
            },
            Extent3d {
                width: u.w,
                height: u.h,
                depth_or_array_layers: 1,
            },
        );
        false
    });
}

/// `WOW_GLYPH_CACHE=1`: one line a second of what the cache holds, the measurement that sizes
/// `super::pack::SHEET_SIZE`.
fn report_cache(atlas: &UiFontAtlas) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static LAST: AtomicU64 = AtomicU64::new(0);
    if std::env::var_os("WOW_GLYPH_CACHE").is_none_or(|v| v == "0") {
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    if LAST.swap(now, Ordering::Relaxed) == now {
        return;
    }
    let e = atlas.lock();
    let (used, total) = e.sheet.occupancy();
    eprintln!(
        "[glyph-cache] {used}/{total} texels ({:.1}%) · {} chars shaped · \
         {} cells rasterized · {} resets · dpi {}",
        100.0 * used as f64 / total as f64,
        e.stats.chars_shaped,
        e.stats.cells_rasterized,
        e.stats.resets,
        e.dpi,
    );
}

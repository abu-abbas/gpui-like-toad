use objc2::{AnyThread as _, MainThreadMarker, MainThreadOnly as _};
use objc2_app_kit::{NSApplication, NSImage, NSImageScaling, NSImageView};
use objc2_foundation::{NSData, NSPoint, NSRect};
use ora_core::store::AppIcon;

pub(crate) fn apply_icon(icon: AppIcon) {
    let Some(main_thread) = MainThreadMarker::new() else {
        return;
    };
    let tile = NSApplication::sharedApplication(main_thread).dockTile();

    match icon {
        AppIcon::Folded => tile.setContentView(None),
        AppIcon::Bolt => {
            let data = NSData::with_bytes(crate::about::icon_png(icon));
            let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) else {
                return;
            };
            let frame = NSRect::new(NSPoint::new(0.0, 0.0), tile.size());
            let view = NSImageView::initWithFrame(NSImageView::alloc(main_thread), frame);
            view.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
            view.setImage(Some(&image));
            tile.setContentView(Some(&view));
        }
    }
    tile.display();
}

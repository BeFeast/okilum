// Quick Look belongs to the Reader window, not an external qlmanage process.
#import <AppKit/AppKit.h>
#import <Quartz/Quartz.h>
#include <stdbool.h>
#include <string.h>

@interface OkilumQuickLook : NSResponder <QLPreviewPanelDataSource, QLPreviewPanelDelegate>
@property(nonatomic, strong) NSURL *url;
@property(nonatomic, weak) NSWindow *owner;
@property(nonatomic, strong) NSResponder *previousResponder;
@property(nonatomic) BOOL enabled;
@end
@implementation OkilumQuickLook
- (BOOL)acceptsPreviewPanelControl:(QLPreviewPanel *)panel {
    return self.enabled && self.owner && self.url;
}
- (void)beginPreviewPanelControl:(QLPreviewPanel *)panel {
    panel.dataSource = self;
    panel.delegate = self;
}
- (void)endPreviewPanelControl:(QLPreviewPanel *)panel {
    if (panel.dataSource == self) panel.dataSource = nil;
    if (panel.delegate == self) panel.delegate = nil;
}
- (NSInteger)numberOfPreviewItemsInPreviewPanel:(QLPreviewPanel *)panel { return self.url ? 1 : 0; }
- (id<QLPreviewItem>)previewPanel:(QLPreviewPanel *)panel previewItemAtIndex:(NSInteger)index { return self.url; }
- (BOOL)previewPanel:(QLPreviewPanel *)panel handleEvent:(NSEvent *)event {
    if (event.type != NSEventTypeKeyDown) return NO;
    if (event.keyCode == 53 || event.keyCode == 49) {
        [panel orderOut:nil];
        [self.owner makeKeyAndOrderFront:nil];
        return YES;
    }
    if ((event.keyCode == 125 || event.keyCode == 126) && self.owner) {
        NSEvent *forward = [NSEvent keyEventWithType:event.type location:NSZeroPoint
            modifierFlags:event.modifierFlags timestamp:event.timestamp
            windowNumber:self.owner.windowNumber context:nil characters:event.characters
            charactersIgnoringModifiers:event.charactersIgnoringModifiers
            isARepeat:event.isARepeat keyCode:event.keyCode];
        [self.owner sendEvent:forward];
        return YES;
    }
    return NO;
}
@end
void *okilum_quicklook_new(void) {
    NSCAssert([NSThread isMainThread], @"Quick Look must run on the main thread");
    OkilumQuickLook *state = [OkilumQuickLook new];
    state.owner = NSApp.keyWindow;
    if (!state.owner) return NULL;
    // QLPreviewPanel discovers its controller through the owning window's
    // responder chain. Preserve AppKit/GPUI responders instead of replacing them.
    state.previousResponder = state.owner.nextResponder;
    state.nextResponder = state.previousResponder;
    state.owner.nextResponder = state;
    return (__bridge_retained void *)state;
}
bool okilum_quicklook_show(void *handle, const char *path) {
    OkilumQuickLook *state = (__bridge OkilumQuickLook *)handle;
    NSString *name = [[NSFileManager defaultManager] stringWithFileSystemRepresentation:path length:strlen(path)];
    if (!name || !state.owner) return false;
    state.url = [NSURL fileURLWithPath:name];
    QLPreviewPanel *panel = [QLPreviewPanel sharedPreviewPanel];
    state.enabled = YES;
    [panel updateController];
    if (panel.currentController != state) return false;
    [panel reloadData];
    [panel orderFront:nil];
    return true;
}
bool okilum_quicklook_visible(void *handle) {
    if (![QLPreviewPanel sharedPreviewPanelExists]) return false;
    QLPreviewPanel *panel = [QLPreviewPanel sharedPreviewPanel];
    return panel.visible && panel.dataSource == (__bridge OkilumQuickLook *)handle;
}
void okilum_quicklook_release(void *handle) {
    OkilumQuickLook *state = (__bridge_transfer OkilumQuickLook *)handle;
    state.enabled = NO;
    // Another component may have inserted a responder since we opened.
    // Remove only our node and keep the rest of the chain intact.
    for (NSResponder *responder = state.owner; responder; responder = responder.nextResponder) {
        if (responder.nextResponder == state) {
            responder.nextResponder = state.nextResponder;
            break;
        }
    }
    if ([QLPreviewPanel sharedPreviewPanelExists]) {
        QLPreviewPanel *panel = [QLPreviewPanel sharedPreviewPanel];
        if (panel.currentController == state) {
            [panel orderOut:nil];
            [panel updateController];
        }
    }
    state.nextResponder = nil;
    state.previousResponder = nil;
}

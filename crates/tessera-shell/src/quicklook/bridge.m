// Quick Look belongs to the Reader window, not an external qlmanage process.
#import <AppKit/AppKit.h>
#import <Quartz/Quartz.h>
#include <stdbool.h>
#include <string.h>

@interface TesseraQuickLook : NSObject <QLPreviewPanelDataSource, QLPreviewPanelDelegate>
@property(nonatomic, strong) NSURL *url;
@property(nonatomic, weak) NSWindow *owner;
@end
@implementation TesseraQuickLook
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
void *tessera_quicklook_new(void) {
    NSCAssert([NSThread isMainThread], @"Quick Look must run on the main thread");
    TesseraQuickLook *state = [TesseraQuickLook new];
    state.owner = NSApp.keyWindow;
    return (__bridge_retained void *)state;
}
bool tessera_quicklook_show(void *handle, const char *path) {
    TesseraQuickLook *state = (__bridge TesseraQuickLook *)handle;
    NSString *name = [[NSFileManager defaultManager] stringWithFileSystemRepresentation:path length:strlen(path)];
    if (!name || !state.owner) return false;
    state.url = [NSURL fileURLWithPath:name];
    QLPreviewPanel *panel = [QLPreviewPanel sharedPreviewPanel];
    panel.dataSource = state;
    panel.delegate = state;
    [panel reloadData];
    [panel orderFront:nil];
    return true;
}
bool tessera_quicklook_visible(void *handle) {
    if (![QLPreviewPanel sharedPreviewPanelExists]) return false;
    QLPreviewPanel *panel = [QLPreviewPanel sharedPreviewPanel];
    return panel.visible && panel.dataSource == (__bridge TesseraQuickLook *)handle;
}
void tessera_quicklook_release(void *handle) {
    TesseraQuickLook *state = (__bridge_transfer TesseraQuickLook *)handle;
    if ([QLPreviewPanel sharedPreviewPanelExists]) {
        QLPreviewPanel *panel = [QLPreviewPanel sharedPreviewPanel];
        if (panel.dataSource == state) {
            [panel orderOut:nil];
            panel.dataSource = nil;
            panel.delegate = nil;
        }
    }
}

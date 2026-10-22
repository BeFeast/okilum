#import <AppKit/AppKit.h>
#import <ImageIO/ImageIO.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>
#import <dlfcn.h>
#import <unistd.h>

// Pre-15 own-window API: captures GPU surfaces without Screen Recording for
// this application's own windows. It is optional, never used on macOS 15+.
typedef CGImageRef (*WindowImage)(CGRect, CGWindowListOption, CGWindowID, CGWindowImageOption);

static NSData *encode_image(CGImageRef image) {
    size_t width = CGImageGetWidth(image), height = CGImageGetHeight(image);
    if (!width || !height || width > 16777216 / height) return nil;
    NSMutableData *data = [NSMutableData data];
    CGImageDestinationRef destination = CGImageDestinationCreateWithData(
        (__bridge CFMutableDataRef)data, CFSTR("public.png"), 1, NULL);
    if (!destination) return nil;
    CGImageDestinationAddImage(destination, image, NULL);
    BOOL ok = CGImageDestinationFinalize(destination);
    CFRelease(destination);
    if (!ok || !data.length || data.length > 8 * 1024 * 1024) return nil;
    return data;
}

unsigned char *okilum_capture_own_window(size_t *length) {
    *length = 0;
    if (![NSThread isMainThread]) return NULL;
    @autoreleasepool {
        NSWindow *window = NSApp.keyWindow;
        if (!window || !window.visible || window.miniaturized || !NSApp.active ||
            ![NSApp.windows containsObject:window] || window.windowNumber <= 0) return NULL;
        CGWindowID windowID = (CGWindowID)window.windowNumber;
        NSData *data = nil;
        if (@available(macOS 15.0, *)) {
            // ScreenCaptureKit needs Screen Recording authorization. Read the
            // existing grant only: never request it or show a system picker.
            if (!CGPreflightScreenCaptureAccess()) return NULL;
            dispatch_semaphore_t done = dispatch_semaphore_create(0);
            __block NSData *captured = nil;
            [SCShareableContent getShareableContentExcludingDesktopWindows:YES onScreenWindowsOnly:YES
                completionHandler:^(SCShareableContent *content, NSError *error) {
                    if (error) { dispatch_semaphore_signal(done); return; }
                    SCWindow *target = nil;
                    for (SCWindow *candidate in content.windows) {
                        if (candidate.windowID == windowID && candidate.owningApplication.processID == getpid()) {
                            target = candidate; break;
                        }
                    }
                    if (!target) { dispatch_semaphore_signal(done); return; }
                    SCContentFilter *filter = [[SCContentFilter alloc] initWithDesktopIndependentWindow:target];
                    SCStreamConfiguration *config = [SCStreamConfiguration new];
                    CGFloat scale = filter.pointPixelScale;
                    CGFloat width = filter.contentRect.size.width * scale;
                    CGFloat height = filter.contentRect.size.height * scale;
                    if (width < 1 || height < 1 || width * height > 16777216) {
                        dispatch_semaphore_signal(done); return;
                    }
                    config.width = (size_t)width;
                    config.height = (size_t)height;
                    config.showsCursor = NO;
                    [SCScreenshotManager captureImageWithFilter:filter configuration:config
                        completionHandler:^(CGImageRef image, NSError *captureError) {
                            if (!captureError && image) captured = encode_image(image);
                            dispatch_semaphore_signal(done);
                        }];
                }];
            if (dispatch_semaphore_wait(done, dispatch_time(DISPATCH_TIME_NOW, 2 * NSEC_PER_SEC))) return NULL;
            data = captured;
        } else {
            WindowImage capture = (WindowImage)dlsym(RTLD_DEFAULT, "CGWindowListCreateImage");
            if (!capture) return NULL;
            CGImageRef image = capture(CGRectNull, kCGWindowListOptionIncludingWindow, windowID,
                kCGWindowImageBoundsIgnoreFraming | kCGWindowImageBestResolution);
            if (!image) return NULL;
            data = encode_image(image);
            CGImageRelease(image);
        }
        if (!data) return NULL;
        unsigned char *result = malloc(data.length);
        if (!result) return NULL;
        memcpy(result, data.bytes, data.length);
        *length = data.length;
        return result;
    }
}
void okilum_capture_free(unsigned char *bytes) { free(bytes); }

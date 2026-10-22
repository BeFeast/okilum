#import <AppKit/AppKit.h>
#import <ImageIO/ImageIO.h>
#import <dlfcn.h>

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

unsigned char *okilum_capture_own_window(size_t *length, int *status) {
    *length = 0;
    *status = 0; // Permission/API unavailable.
    if (![NSThread isMainThread]) { *status = 1; return NULL; }
    @autoreleasepool {
        NSWindow *window = NSApp.keyWindow;
        if (!window || !window.visible || window.miniaturized || !NSApp.active ||
            ![NSApp.windows containsObject:window] || window.windowNumber <= 0) { *status = 2; return NULL; }
        CGWindowID windowID = (CGWindowID)window.windowNumber;
        NSData *data = nil;
        if (@available(macOS 15.0, *)) {
            // The supported replacement (ScreenCaptureKit) requires Screen
            // Recording authorization. The synchronous least-permission API
            // deliberately stays attach-only rather than requesting that grant
            // or waiting for async callbacks on the UI thread.
            return NULL;
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

// QLThumbnailGenerator owns provider work. No NSView is embedded in GPUI.
#import <AppKit/AppKit.h>
#import <QuickLookThumbnailing/QuickLookThumbnailing.h>
#include <stdlib.h>
#include <string.h>

@interface TesseraThumbnail : NSObject
@property(nonatomic, strong) NSLock *lock;
@property(nonatomic, strong) QLThumbnailGenerationRequest *request;
@property(nonatomic, strong) NSData *png;
@property(nonatomic) NSInteger status; // 0 pending, 1 ready, -1 failed/cancelled
@end
@implementation TesseraThumbnail
@end

void *tessera_thumbnail_start(const char *path) {
    @autoreleasepool {
        NSString *name = [[NSFileManager defaultManager]
            stringWithFileSystemRepresentation:path length:strlen(path)];
        if (!name) return NULL;
        TesseraThumbnail *state = [TesseraThumbnail new];
        state.lock = [NSLock new];
        // 1024 points at 2x: adequate for the Reader column on Retina, bounded
        // to 2048 physical pixels. Request a real thumbnail, never a file icon.
        state.request = [[QLThumbnailGenerationRequest alloc]
            initWithFileAtURL:[NSURL fileURLWithPath:name]
            size:CGSizeMake(1024, 1024) scale:2.0
            representationTypes:QLThumbnailGenerationRequestRepresentationTypeThumbnail];
        __weak TesseraThumbnail *weakState = state;
        [[QLThumbnailGenerator sharedGenerator]
            generateBestRepresentationForRequest:state.request
            completionHandler:^(QLThumbnailRepresentation *representation, NSError *error) {
                dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^{
                  @autoreleasepool {
                    TesseraThumbnail *state = weakState;
                    if (!state) return;
                    [state.lock lock];
                    BOOL cancelled = state.status != 0;
                    [state.lock unlock];
                    if (cancelled) return;
                    CGImageRef image = representation.CGImage;
                    NSData *png = nil;
                    if (!error && image &&
                        representation.type == QLThumbnailRepresentationTypeThumbnail &&
                        CGImageGetWidth(image) > 0 && CGImageGetHeight(image) > 0 &&
                        CGImageGetWidth(image) <= 2048 && CGImageGetHeight(image) <= 2048) {
                        NSBitmapImageRep *bitmap = [[NSBitmapImageRep alloc] initWithCGImage:image];
                        png = [bitmap representationUsingType:NSBitmapImageFileTypePNG properties:@{}];
                    }
                    [state.lock lock];
                    if (state.status == 0) {
                        if (png.length > 0 && png.length <= 32 * 1024 * 1024) {
                            state.png = png;
                            state.status = 1;
                        } else {
                            state.status = -1;
                        }
                    }
                    [state.lock unlock];
                  }
                });
            }];
        return (__bridge_retained void *)state;
    }
}

// On success the caller owns the malloc buffer and frees it with the paired API.
int tessera_thumbnail_poll(void *handle, unsigned char **bytes, size_t *length) {
    @autoreleasepool {
        TesseraThumbnail *state = (__bridge TesseraThumbnail *)handle;
        [state.lock lock];
        int status = (int)state.status;
        if (status == 1) {
            *length = state.png.length;
            *bytes = malloc(*length);
            if (*bytes) memcpy(*bytes, state.png.bytes, *length);
            else status = -1;
            state.png = nil;
            state.status = -1; // take once
        }
        [state.lock unlock];
        return status;
    }
}

void tessera_thumbnail_free_bytes(unsigned char *bytes) { free(bytes); }

// Cancellation consumes our retain. The callback only has a weak reference,
// so even a provider that never completes cannot keep the state alive.
void tessera_thumbnail_cancel(void *handle) {
    @autoreleasepool {
        TesseraThumbnail *state = (__bridge_transfer TesseraThumbnail *)handle;
        [state.lock lock];
        state.status = -1;
        state.png = nil;
        [state.lock unlock];
        [[QLThumbnailGenerator sharedGenerator] cancelRequest:state.request];
    }
}

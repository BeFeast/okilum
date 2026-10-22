// CI-only GPU positive control. Never linked into shipping builds.
#import <AppKit/AppKit.h>
#import <QuartzCore/CAMetalLayer.h>
#import <Metal/Metal.h>
static NSWindow *control;
int okilum_capture_probe_open(void) {
    [NSApplication sharedApplication];
    [NSApp setActivationPolicy:NSApplicationActivationPolicyRegular];
    control = [[NSWindow alloc] initWithContentRect:NSMakeRect(100, 100, 160, 120)
        styleMask:NSWindowStyleMaskTitled backing:NSBackingStoreBuffered defer:NO];
    id<MTLDevice> device = MTLCreateSystemDefaultDevice();
    if (!device) return 0;
    CAMetalLayer *layer = [CAMetalLayer layer];
    layer.device = device;
    layer.pixelFormat = MTLPixelFormatBGRA8Unorm;
    layer.framebufferOnly = YES;
    layer.frame = control.contentView.bounds;
    layer.drawableSize = CGSizeMake(160, 120);
    control.contentView.wantsLayer = YES;
    control.contentView.layer = layer;
    [control makeKeyAndOrderFront:nil];
    [NSApp activateIgnoringOtherApps:YES];
    NSDate *until = [NSDate dateWithTimeIntervalSinceNow:0.2];
    while ([until timeIntervalSinceNow] > 0) {
        NSEvent *event = [NSApp nextEventMatchingMask:NSEventMaskAny untilDate:until
            inMode:NSDefaultRunLoopMode dequeue:YES];
        if (event) [NSApp sendEvent:event];
    }
    id<CAMetalDrawable> drawable = [layer nextDrawable];
    if (!drawable) return 0;
    MTLRenderPassDescriptor *pass = [MTLRenderPassDescriptor renderPassDescriptor];
    pass.colorAttachments[0].texture = drawable.texture;
    pass.colorAttachments[0].loadAction = MTLLoadActionClear;
    pass.colorAttachments[0].storeAction = MTLStoreActionStore;
    pass.colorAttachments[0].clearColor = MTLClearColorMake(0.9, 0.1, 0.2, 1);
    id<MTLCommandQueue> queue = [device newCommandQueue];
    id<MTLCommandBuffer> command = [queue commandBuffer];
    id<MTLRenderCommandEncoder> encoder = [command renderCommandEncoderWithDescriptor:pass];
    [encoder endEncoding];
    [command presentDrawable:drawable];
    [command commit];
    [command waitUntilCompleted];
    return command.status == MTLCommandBufferStatusCompleted && control.isKeyWindow;
}
void okilum_capture_probe_close(void) { [control orderOut:nil]; control = nil; }

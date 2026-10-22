// Native hosted regression: real NSApp/NSWindow/WKWebView responder chain.
// Local HTML only; no T3, OAuth, pairing tokens or production browser data.
#import "../../crates/okilum-shell/src/reader_manager_focus/bridge.m"

#define REQUIRE(condition, message) do { if (!(condition)) { NSLog(@"FAIL: %@", message); exit(1); } } while (0)

// Match Wry's child performKeyEquivalent override, without modifying Wry.
@interface ChildWebView : WKWebView
@end
@implementation ChildWebView
- (BOOL)performKeyEquivalent:(NSEvent *)event { (void)event; return NO; }
@end
@interface HostView : NSView
@property(nonatomic) NSUInteger pastes;
@end
@implementation HostView
- (BOOL)acceptsFirstResponder { return YES; }
- (void)paste:(id)sender { (void)sender; self.pastes++; }
@end

static id javascript(WKWebView *view, NSString *source) {
    __block BOOL done = NO;
    __block id value = nil;
    __block NSError *failure = nil;
    [view evaluateJavaScript:source completionHandler:^(id result, NSError *error) {
        value = result; failure = error; done = YES;
    }];
    NSDate *deadline = [NSDate dateWithTimeIntervalSinceNow:10];
    while (!done && deadline.timeIntervalSinceNow > 0) {
        [NSRunLoop.currentRunLoop runMode:NSDefaultRunLoopMode beforeDate:[NSDate dateWithTimeIntervalSinceNow:0.01]];
    }
    REQUIRE(done && !failure, ([NSString stringWithFormat:@"JavaScript fixture failed: %@", failure]));
    return value;
}
static void waitFor(WKWebView *view, NSString *expression) {
    NSDate *deadline = [NSDate dateWithTimeIntervalSinceNow:10];
    while (deadline.timeIntervalSinceNow > 0) {
        if ([javascript(view, expression) boolValue]) return;
        [NSRunLoop.currentRunLoop runMode:NSDefaultRunLoopMode beforeDate:[NSDate dateWithTimeIntervalSinceNow:0.02]];
    }
    REQUIRE(NO, expression);
}
int main(void) {
    @autoreleasepool {
        [NSApplication sharedApplication];
        [NSApp setActivationPolicy:NSApplicationActivationPolicyAccessory];
        NSMenu *original = [[NSMenu alloc] initWithTitle:@"Fixture"];
        NSApp.mainMenu = original;
        [NSApp finishLaunching];
        NSWindow *window = [[NSWindow alloc] initWithContentRect:NSMakeRect(0, 0, 600, 400)
            styleMask:NSWindowStyleMaskTitled backing:NSBackingStoreBuffered defer:NO];
        window.releasedWhenClosed = NO;
        HostView *host = [[HostView alloc] initWithFrame:NSMakeRect(0, 0, 600, 400)];
        window.contentView = host;
        WKWebViewConfiguration *configuration = [WKWebViewConfiguration new];
        configuration.websiteDataStore = WKWebsiteDataStore.nonPersistentDataStore;
        WKWebView *view = [[ChildWebView alloc] initWithFrame:host.bounds configuration:configuration];
        [host addSubview:view];
        [window makeKeyAndOrderFront:nil];
        [NSApp activateIgnoringOtherApps:YES];
        [view loadHTMLString:@"<html><body><input id='token' aria-label='Pairing token'></body></html>" baseURL:nil];
        waitFor(view, @"!!document.getElementById('token')");
        REQUIRE([window makeFirstResponder:view], @"WKWebView must accept first responder");
        javascript(view, @"document.getElementById('token').focus(); true");
        void *handle = okilum_manager_focus_attach((__bridge void *)view, (__bridge void *)host, NULL, NULL);
        OkilumManagerFocus *router = (__bridge OkilumManagerFocus *)handle;
        // Activation is asynchronous for a command-line AppKit process. Pump
        // the native loop before treating absence of focus as product evidence.
        NSDate *focusDeadline = [NSDate dateWithTimeIntervalSinceNow:5];
        while (!router.focused && focusDeadline.timeIntervalSinceNow > 0) {
            [window makeKeyWindow];
            [NSRunLoop.currentRunLoop runMode:NSDefaultRunLoopMode beforeDate:[NSDate dateWithTimeIntervalSinceNow:0.02]];
        }
        NSLog(@"Focus control: active=%d keyWindow=%@ fixture=%@ firstResponder=%@ viewWindow=%@ hidden=%d",
            NSApp.active, NSApp.keyWindow, window, window.firstResponder, view.window, view.hidden);
        REQUIRE(router.focused, @"real WKWebView must own the key window's native responder");
        NSLog(@"Native responder: %@; paste target: %@", window.firstResponder,
            [NSApp targetForAction:@selector(paste:) to:nil from:router]);
        [NSPasteboard.generalPasteboard clearContents];
        REQUIRE([NSPasteboard.generalPasteboard setString:@"okilum-native-paste-1510" forType:NSPasteboardTypeString], @"clipboard positive control");
        NSEvent *paste = [NSEvent keyEventWithType:NSEventTypeKeyDown location:NSZeroPoint
            modifierFlags:NSEventModifierFlagCommand timestamp:0 windowNumber:window.windowNumber
            context:nil characters:@"v" charactersIgnoringModifiers:@"v" isARepeat:NO keyCode:9];
        // Production monitor callback, with a real AppKit responder chain. The
        // child override returns NO, so merely forwarding performKeyEquivalent
        // through Wry cannot make this pass.
        REQUIRE([router event:paste] == nil, @"native editing must accept Cmd+V");
        waitFor(view, @"document.getElementById('token').value === 'okilum-native-paste-1510'");
        REQUIRE(host.pastes == 0, @"Cmd+V must not reach the GPUI host");
        javascript(view, @"document.getElementById('token').value=''; true");
        [router refresh];
        NSMenu *edit = [NSApp.mainMenu itemWithTitle:@"Edit"].submenu;
        NSMenuItem *item = [edit itemWithTitle:@"Paste"];
        REQUIRE(item && item.target == router, @"native Edit menu owns an explicit browser target");
        REQUIRE([NSApp sendAction:item.action to:item.target from:item], @"NSApp must deliver menu paste");
        waitFor(view, @"document.getElementById('token').value === 'okilum-native-paste-1510'");
        REQUIRE(host.pastes == 0, @"menu Paste must not reach GPUI");
        okilum_manager_focus_detach(handle);
        REQUIRE([window makeFirstResponder:host], @"focus returns outside webview");
        REQUIRE([NSApp sendAction:@selector(paste:) to:nil from:nil], @"outside native responder receives paste");
        REQUIRE(host.pastes == 1, @"positive control: host paste works after teardown");
        [window close];
        NSLog(@"PASS: real NSApp Cmd+V + Edit/Paste deliver to WKWebView, outside focus restores host");
    }
    return 0;
}

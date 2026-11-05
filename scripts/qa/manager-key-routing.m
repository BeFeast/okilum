// Hosted, headless regression for the production AppKit entry-point hooks.
// Boundary doubles supply native focus and record host/WebKit dispatch; no
// NSApplication activation, real window, clipboard or production T3 profile.
#import "bridge.m"

#define REQUIRE(condition, message) do { if (!(condition)) { NSLog(@"FAIL: %@", message); exit(1); } } while (0)

@interface NavigationFixture : NSObject <WKNavigationDelegate>
@property(nonatomic) NSUInteger policies;
@property(nonatomic) NSUInteger finishes;
@property(nonatomic) NSUInteger failures;
@end
@implementation NavigationFixture
- (void)webView:(WKWebView *)view decidePolicyForNavigationAction:(WKNavigationAction *)action
    decisionHandler:(void (^)(WKNavigationActionPolicy))handler {
    (void)view; (void)action; self.policies++; handler(WKNavigationActionPolicyAllow);
}
- (void)webView:(WKWebView *)view didFinishNavigation:(WKNavigation *)navigation {
    (void)view; (void)navigation; self.finishes++;
}
- (void)webView:(WKWebView *)view didFailProvisionalNavigation:(WKNavigation *)navigation withError:(NSError *)error {
    (void)view; (void)navigation; (void)error; self.failures++;
}
@end

static void NavigationFailure(void *context, const char *json) {
    NSMutableArray *events = (__bridge NSMutableArray *)context;
    NSData *data = [[NSString stringWithUTF8String:json] dataUsingEncoding:NSUTF8StringEncoding];
    [events addObject:[NSJSONSerialization JSONObjectWithData:data options:0 error:NULL]];
}

@interface NavigationWebViewFixture : NSObject
@property(nonatomic, strong) NSURL *URL;
@end
@implementation NavigationWebViewFixture
@end

static void CheckNavigationProxy(void) {
    NavigationFixture *original = [NavigationFixture new];
    OkilumManagerNavigation *proxy = [OkilumManagerNavigation new];
    NSMutableArray *events = [NSMutableArray new];
    proxy.original = original;
    proxy.failed = NavigationFailure;
    proxy.context = (__bridge void *)events;
    NavigationWebViewFixture *fixture = [NavigationWebViewFixture new];
    fixture.URL = [NSURL URLWithString:@"https://t3.example/env/thread"];
    WKWebView *view = (WKWebView *)fixture;
    WKNavigationAction *action = (WKNavigationAction *)[NSObject new];
    REQUIRE([proxy respondsToSelector:@selector(webView:decidePolicyForNavigationAction:decisionHandler:)],
        @"proxy advertises Wry policy callbacks");
    __block BOOL allowed = NO;
    [proxy webView:view decidePolicyForNavigationAction:action decisionHandler:^(WKNavigationActionPolicy policy) {
        allowed = policy == WKNavigationActionPolicyAllow;
    }];
    [proxy webView:view didFinishNavigation:nil];
    NSError *error = [NSError errorWithDomain:NSURLErrorDomain code:NSURLErrorCannotConnectToHost
        userInfo:@{NSURLErrorFailingURLStringErrorKey: @"https://t3.example/env/thread?token=fixture"}];
    [proxy webView:view didFailProvisionalNavigation:nil withError:error];
    [proxy webView:view didFailNavigation:nil withError:error];
    [proxy webViewWebContentProcessDidTerminate:view];
    REQUIRE(allowed && original.policies == 1 && original.finishes == 1 && original.failures == 1,
        @"proxy preserves original policy, completion and failure callbacks");
    REQUIRE(events.count == 3 && [events[0][@"domain"] isEqual:NSURLErrorDomain]
        && [events[0][@"code"] integerValue] == NSURLErrorCannotConnectToHost
        && [events[2][@"domain"] isEqual:@"WebContentProcess"], @"both failures and termination reach Rust");
    proxy.failed = NULL;
    proxy.context = NULL;
    [proxy webView:view didFailNavigation:nil withError:error];
    REQUIRE(events.count == 3, @"detached proxy cannot call the released Rust context");
}

@interface KeyFixtureResponder : NSObject
@property(nonatomic) NSUInteger downs;
@property(nonatomic) NSUInteger ups;
@property(nonatomic) NSUInteger flags;
@end
@implementation KeyFixtureResponder
- (void)keyDown:(NSEvent *)event { (void)event; self.downs++; }
- (void)keyUp:(NSEvent *)event { (void)event; self.ups++; }
- (void)flagsChanged:(NSEvent *)event { (void)event; self.flags++; }
@end

@interface KeyFixtureWindow : NSObject
@property(nonatomic, strong) NSResponder *firstResponder;
@property(nonatomic) NSUInteger hostEquivalents;
- (BOOL)performKeyEquivalent:(NSEvent *)event;
@end
@implementation KeyFixtureWindow
- (BOOL)performKeyEquivalent:(NSEvent *)event {
    (void)event; self.hostEquivalents++; return YES;
}
@end

@interface KeyFixtureWebView : NSObject
@property(nonatomic, strong) NSWindow *window;
@property(nonatomic, getter=isHidden) BOOL hidden;
@end
@implementation KeyFixtureWebView
- (WKWebViewConfiguration *)configuration { return nil; }
- (void)evaluateJavaScript:(NSString *)source completionHandler:(void (^)(id, NSError *))done {
    (void)source; if (done) done(nil, nil);
}
@end

@interface KeyFixtureApp : NSObject
@property(nonatomic) NSUInteger hostEvents;
- (void)sendEvent:(NSEvent *)event;
- (BOOL)sendAction:(SEL)action to:(id)target from:(id)sender;
@end
@implementation KeyFixtureApp
- (void)sendEvent:(NSEvent *)event { (void)event; self.hostEvents++; }
- (BOOL)sendAction:(SEL)action to:(id)target from:(id)sender {
    (void)action; (void)target; (void)sender; return YES;
}
@end

@interface KeyFixtureEvent : NSObject
@property(nonatomic) NSEventType type;
@property(nonatomic) NSEventModifierFlags modifierFlags;
@property(nonatomic) unsigned short keyCode;
@property(nonatomic, copy) NSString *charactersIgnoringModifiers;
@property(nonatomic, strong) NSWindow *window;
@end
@implementation KeyFixtureEvent
@end

@interface KeyFixtureRouter : OkilumManagerFocus
@property(nonatomic) BOOL ownsFocus;
@property(nonatomic, strong) NSMutableArray<NSString *> *edits;
@end
@implementation KeyFixtureRouter
- (BOOL)focused { return self.ownsFocus; }
- (void)refresh {
    OkilumFocusCycle cycle = self.focusCycle;
    if (FocusCycleSet(&cycle, self.focused)) {
        self.focusCycle = cycle;
        KeyCancelCommands(self);
    }
}
- (void)performNativeCommand:(NSEvent *)event before:(NSDictionary *)before
    serial:(NSUInteger)serial generation:(NSUInteger)generation activity:(NSUInteger)activity {
    (void)before; (void)generation; (void)activity;
    // The real dispatcher/queue must still accept all six editing commands.
    // WebKit editing and the guarded JS fallback have their separate fixtures.
    [self.edits addObject:KeyAction(event)];
    KeyFinish(self, serial, @{});
}
@end

int main(void) {
    CheckNavigationProxy();
    @autoreleasepool {
        NSApplication *savedApp = NSApp;
        KeyFixtureApp *app = [KeyFixtureApp new];
        NSApp = (NSApplication *)app;
        KeyFixtureResponder *content = [KeyFixtureResponder new];
        KeyFixtureWindow *window = [KeyFixtureWindow new];
        window.firstResponder = (NSResponder *)content;
        KeyFixtureWebView *view = [KeyFixtureWebView new];
        view.window = (NSWindow *)window;
        KeyFixtureRouter *router = [KeyFixtureRouter new];
        router.webview = (WKWebView *)view;
        router.edits = [NSMutableArray new];
        KeyInstallRouter(router); // Install the actual production hooks.
        KeyFixtureEvent *key = [KeyFixtureEvent new];
        key.type = NSEventTypeKeyDown;
        key.modifierFlags = NSEventModifierFlagCommand;
        key.charactersIgnoringModifiers = @"k";
        key.window = (NSWindow *)window;
        NSEvent *event = (NSEvent *)key;

        // Positive controls: both original host entry points really fire.
        [app sendEvent:event];
        REQUIRE([window performKeyEquivalent:event], @"outside host key equivalent");
        REQUIRE(app.hostEvents == 1 && window.hostEquivalents == 1, @"outside host controls fired");
        router.ownsFocus = YES;
        for (NSNumber *modifiers in @[@(NSEventModifierFlagCommand),
                @(NSEventModifierFlagCommand | NSEventModifierFlagShift),
                @(NSEventModifierFlagCommand | NSEventModifierFlagOption | NSEventModifierFlagShift)]) {
            for (NSString *letter in @[@"k", @"f", @"p", @"\uF702", @"\x7f", @"v"]) {
                key.charactersIgnoringModifiers = letter;
                key.keyCode = [letter isEqual:@"\uF702"] ? 123 : ([letter isEqual:@"\x7f"] ? 51 : 0);
                key.modifierFlags = modifiers.unsignedIntegerValue;
                if (KeyAction(event)) continue; // Primary edits are checked below.
                NSUInteger downs = content.downs;
                [app sendEvent:event];
                REQUIRE(content.downs == downs + 1, @"NSApp must deliver non-primary Cmd shortcut to web content exactly once");
                REQUIRE([window performKeyEquivalent:event], @"window key equivalent is owned by web content");
                REQUIRE(content.downs == downs + 2, @"direct window key equivalent must reach web content exactly once");
                REQUIRE(app.hostEvents == 1 && window.hostEquivalents == 1, @"Okilum must not steal a focused webview shortcut");
            }
        }
        key.modifierFlags = NSEventModifierFlagCommand;
        for (NSString *letter in @[@"v", @"c", @"x", @"a", @"z"]) {
            key.charactersIgnoringModifiers = letter;
            [app sendEvent:event];
        }
        key.modifierFlags |= NSEventModifierFlagShift;
        [app sendEvent:event];
        REQUIRE(([router.edits isEqualToArray:@[@"paste:", @"copy:", @"cut:", @"selectAll:", @"undo:", @"redo:"]]),
            @"all six primary commands retain their ordered editing dispatcher");
        key.type = NSEventTypeKeyUp;
        [app sendEvent:event];
        key.type = NSEventTypeFlagsChanged;
        [app sendEvent:event];
        REQUIRE(content.ups == 1 && content.flags == 1, @"key-up and modifier transitions reach web content");
        REQUIRE(app.hostEvents == 1, @"editing and key transitions bypass host dispatch");

        // A hook installed on NSWindow must not hijack a different window.
        KeyFixtureWindow *other = [KeyFixtureWindow new];
        key.type = NSEventTypeKeyDown;
        key.charactersIgnoringModifiers = @"k";
        key.window = (NSWindow *)other;
        [app sendEvent:event];
        REQUIRE([other performKeyEquivalent:event] && other.hostEquivalents == 1 && app.hostEvents == 2,
            @"another window retains host routing");
        key.window = (NSWindow *)window;
        router.nativeRouting = YES;
        [app sendEvent:event];
        REQUIRE(app.hostEvents == 3, @"native editing replay remains exempt from interception");
        router.nativeRouting = NO;
        router.ownsFocus = NO;
        [app sendEvent:event];
        REQUIRE([window performKeyEquivalent:event] && window.hostEquivalents == 2 && app.hostEvents == 4,
            @"leaving web content restores both original entry points");
        router.ownsFocus = YES;
        NSUInteger downs = content.downs;
        [app sendEvent:event];
        REQUIRE(content.downs == downs + 1 && app.hostEvents == 4, @"re-entry re-arms keyboard ownership");
        KeyRemoveRouter(router);
        NSApp = savedApp;
        puts("PASS: production app/window hooks isolate all focused keyboard events, retain six edit commands and restore host routing");
    }
    return 0;
}

// Public AppKit responder APIs only; no GPUI/Wry subclass patches or JS paste.
#import <AppKit/AppKit.h>
#import <WebKit/WebKit.h>
#include <stdbool.h>
#include <string.h>
#import <objc/runtime.h>

// Shared by the event monitor and hosted selector regression tests.
const char *okilum_manager_edit_action(const char *key, bool command, bool shift, bool control, bool option) {
    if (!command || control || option || !key) return NULL;
    if (shift && strcmp(key, "z") != 0) return NULL;
    if (strcmp(key, "v") == 0) return "paste:";
    if (strcmp(key, "c") == 0) return "copy:";
    if (strcmp(key, "x") == 0) return "cut:";
    if (strcmp(key, "a") == 0) return "selectAll:";
    if (strcmp(key, "z") == 0) return shift ? "redo:" : "undo:";
    return NULL;
}

typedef void (*OkilumFocusChanged)(void *, int);
static void KeyLog(NSString *format, ...) NS_FORMAT_FUNCTION(1,2);
static void KeyInstallRouter(id router);
static void KeyRemoveRouter(id router);

@interface OkilumManagerFocus : NSObject
@property(nonatomic, weak) WKWebView *webview;
@property(nonatomic, weak) NSView *parent;
@property(nonatomic) OkilumFocusChanged focusChanged;
@property(nonatomic) void *context;
@property(nonatomic, strong) id monitor;
@property(nonatomic, strong) NSArray *observers;
@property(nonatomic, strong) NSMenu *installedMenu;
@property(nonatomic, strong) NSMenuItem *editItem;
@property(nonatomic, strong) NSMenu *previousSubmenu;
@property(nonatomic) BOOL addedItem;
@property(nonatomic) BOOL nativeRouting;
@property(nonatomic) BOOL keyBusy;
@property(nonatomic, copy) NSString *keyToken;
@property(nonatomic, strong) NSMutableArray *keyQueue;
- (BOOL)focused;
- (void)refresh;
- (void)detach;
- (BOOL)dispatchEditAction:(SEL)action sender:(id)sender;
- (void)editCommand:(NSMenuItem *)item;

@end

@interface OkilumManagerFocus (Keys)
- (BOOL)handleCommandEvent:(NSEvent *)event;
- (void)processKeyQueue;
- (void)finishKeyCommand;
- (void)pageCommand:(NSString *)action token:(NSString *)token;
@end

@implementation OkilumManagerFocus
- (BOOL)focused {
    WKWebView *view = self.webview;
    NSWindow *window = view.window;
    NSResponder *responder = window.firstResponder;
    return view && !view.hidden && window && NSApp.keyWindow == window
        && [responder isKindOfClass:NSView.class]
        && [(NSView *)responder isDescendantOf:view];
}
- (void)restoreMenu {
    if (!self.editItem) return;
    if (self.addedItem) {
        if ([self.installedMenu.itemArray containsObject:self.editItem]) {
            [self.installedMenu removeItem:self.editItem];
        }
    } else {
        self.editItem.submenu = self.previousSubmenu;
    }
    self.editItem = nil;
    self.previousSubmenu = nil;
    self.installedMenu = nil;
    self.addedItem = NO;
}
- (void)refresh {
    if (![self focused]) { [self restoreMenu]; return; }
    NSMenu *menu = NSApp.mainMenu;
    if (!menu) return;
    if (self.editItem && self.installedMenu == menu) return;
    [self restoreMenu];
    NSMenuItem *edit = [menu itemWithTitle:@"Edit"];
    self.addedItem = !edit;
    if (!edit) {
        edit = [[NSMenuItem alloc] initWithTitle:@"Edit" action:NULL keyEquivalent:@""];
        [menu insertItem:edit atIndex:MIN((NSInteger)2, menu.numberOfItems)];
    }
    self.previousSubmenu = edit.submenu;
    NSMenu *native = [[NSMenu alloc] initWithTitle:@"Edit"];
    NSArray<NSString *> *titles = @[@"Undo", @"Redo", @"Cut", @"Copy", @"Paste", @"Select All"];
    NSArray<NSString *> *actions = @[@"undo:", @"redo:", @"cut:", @"copy:", @"paste:", @"selectAll:"];
    NSArray<NSString *> *keys = @[@"z", @"z", @"x", @"c", @"v", @"a"];
    for (NSUInteger i = 0; i < titles.count; i++) {
        NSMenuItem *item = [[NSMenuItem alloc] initWithTitle:titles[i]
            action:NSSelectorFromString(actions[i]) keyEquivalent:keys[i]];
        item.keyEquivalentModifierMask = NSEventModifierFlagCommand
            | (i == 1 ? NSEventModifierFlagShift : 0);
        // Route menus through the same bounded WebKit responder lookup as keys.
        // A nil target can resolve to the surrounding GPUI window/menu delegate.
        item.representedObject = actions[i];
        item.action = @selector(editCommand:);
        item.target = self;
        [native addItem:item];
        if (i == 1) [native addItem:NSMenuItem.separatorItem];
    }
    edit.submenu = native;
    self.editItem = edit;
    self.installedMenu = menu;
}
- (BOOL)dispatchEditAction:(SEL)action sender:(id)sender {
    if (![self focused]) return NO;
    WKWebView *view = self.webview;
    // The focused WKContentView may implement editing even when WKWebView does
    // not. Never let lookup escape to the GPUI parent or its application actions.
    for (NSResponder *target = view.window.firstResponder; target; target = target.nextResponder) {
        BOOL inside = target == view || ([target isKindOfClass:NSView.class]
            && [(NSView *)target isDescendantOf:view]);
        if (!inside) break;
        if ([target respondsToSelector:action]) {
            BOOL delivered = [NSApp sendAction:action to:target from:sender];
            KeyLog(@"native edit action=%@ target=%@ delivered=%d", NSStringFromSelector(action), NSStringFromClass(target.class), delivered);
            if (delivered) return YES;
        }
        if (target == view) break;
    }
    return NO;
}
- (void)editCommand:(NSMenuItem *)item {
    KeyLog(@"menu hit title=%@ action=%@ target=%@", item.title, item.representedObject, NSStringFromClass(self.class));
    if (self.nativeRouting) {
        [self dispatchEditAction:NSSelectorFromString(item.representedObject) sender:item];
        return;
    }
    NSEvent *event = [NSEvent keyEventWithType:NSEventTypeKeyDown location:NSZeroPoint
        modifierFlags:item.keyEquivalentModifierMask timestamp:NSProcessInfo.processInfo.systemUptime
        windowNumber:self.webview.window.windowNumber context:nil characters:item.keyEquivalent
        charactersIgnoringModifiers:item.keyEquivalent isARepeat:NO keyCode:0];
    [self handleCommandEvent:event];
}
- (BOOL)validateMenuItem:(NSMenuItem *)item {
    (void)item;
    return [self focused];
}
- (void)returnToParent {
    if ([self focused]) [self.webview.window makeFirstResponder:self.parent];
    [self restoreMenu];
}
- (NSEvent *)event:(NSEvent *)event {
    WKWebView *view = self.webview;
    NSWindow *window = view.window;
    if (!view || view.hidden || !window || event.window != window) return event;
    if (event.type == NSEventTypeLeftMouseDown || event.type == NSEventTypeRightMouseDown) {
        NSView *content = window.contentView;
        NSView *hit = [content hitTest:[content.superview convertPoint:event.locationInWindow fromView:nil]];
        if (hit && [hit isDescendantOf:view]) {
            if (![self focused]) [window makeFirstResponder:view];
            if (self.focusChanged) self.focusChanged(self.context, 1);
            [self refresh];
        } else {
            [self returnToParent];
            if (self.focusChanged) self.focusChanged(self.context, 0);
        }
        return event; // WebKit receives the original click, selection and drag.
    }
    if (![self focused]) { [self refresh]; return event; }
    if (event.type == NSEventTypeKeyDown && event.keyCode == 53) {
        [self returnToParent];
        if (self.focusChanged) self.focusChanged(self.context, 2);
        return nil; // Explicit escape hatch; do not also dismiss Reader UI.
    }
    NSResponder *responder = window.firstResponder;
    if (event.type == NSEventTypeKeyDown) {
        NSEventModifierFlags flags = event.modifierFlags & NSEventModifierFlagDeviceIndependentFlagsMask;
        const char *action = okilum_manager_edit_action(event.charactersIgnoringModifiers.lowercaseString.UTF8String,
            (flags & NSEventModifierFlagCommand) != 0, (flags & NSEventModifierFlagShift) != 0,
            (flags & NSEventModifierFlagControl) != 0, (flags & NSEventModifierFlagOption) != 0);
        if (action) {
            KeyLog(@"local event monitor action=%s nativeRouting=%d firstResponder=%@", action, self.nativeRouting, NSStringFromClass(window.firstResponder.class));
            if (self.nativeRouting) return event;
            if ([self handleCommandEvent:event]) return nil;
            return event;
        }
        // Bypass the child Wry performKeyEquivalent override and GPUI dispatch.
        // WKContentView interprets native input/IME, never a synthetic JS event.
        [responder keyDown:event];
    } else if (event.type == NSEventTypeKeyUp) {
        [responder keyUp:event];
    } else if (event.type == NSEventTypeFlagsChanged) {
        [responder flagsChanged:event];
    } else {
        return event;
    }
    return nil;
}
- (void)detach {
    KeyRemoveRouter(self);
    [self.keyQueue removeAllObjects];
    [self returnToParent];
    if (self.monitor) [NSEvent removeMonitor:self.monitor];
    for (id observer in self.observers) [NSNotificationCenter.defaultCenter removeObserver:observer];
    self.monitor = nil;
    self.observers = nil;
    self.focusChanged = NULL;
    self.context = NULL;
    self.webview = nil;
    self.parent = nil;
}
@end

#import "keys.inc"

void *okilum_manager_focus_attach(void *webview, void *parent, void (*focusChanged)(void *, int), void *context) {
    NSCAssert(NSThread.isMainThread, @"Manager focus must attach on the main thread");
    OkilumManagerFocus *router = [OkilumManagerFocus new];
    router.webview = (__bridge WKWebView *)webview;
    router.parent = (__bridge NSView *)parent;
    router.focusChanged = focusChanged;
    router.context = context;
    KeyInstallRouter(router);
    __weak OkilumManagerFocus *weakRouter = router;
    router.monitor = [NSEvent addLocalMonitorForEventsMatchingMask:
        NSEventMaskLeftMouseDown | NSEventMaskRightMouseDown | NSEventMaskKeyDown
        | NSEventMaskKeyUp | NSEventMaskFlagsChanged
        handler:^NSEvent *(NSEvent *event) {
            OkilumManagerFocus *strongRouter = weakRouter;
            return strongRouter ? [strongRouter event:event] : event;
        }];
    NSMutableArray *observers = [NSMutableArray new];
    for (NSNotificationName name in @[NSWindowDidUpdateNotification, NSWindowDidBecomeKeyNotification, NSWindowDidResignKeyNotification]) {
        [observers addObject:[NSNotificationCenter.defaultCenter addObserverForName:name
            object:nil queue:nil usingBlock:^(NSNotification *notification) {
                (void)notification;
                [weakRouter refresh];
            }]];
    }
    router.observers = observers;
    [router refresh];
    return (__bridge_retained void *)router;
}
bool okilum_manager_focus_is_focused(void *handle) {
    return [(__bridge OkilumManagerFocus *)handle focused];
}
void okilum_manager_focus_refresh(void *handle) {
    [(__bridge OkilumManagerFocus *)handle refresh];
}
void okilum_manager_focus_detach(void *handle) {
    OkilumManagerFocus *router = (__bridge_transfer OkilumManagerFocus *)handle;
    [router detach];
}

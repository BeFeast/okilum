// App-owned instrumentation of public AppKit entry points. No GPUI/Wry source
// patch, private WebKit selector or clipboard JavaScript permission bypass.
#include "page_keys.inc"

static NSHashTable<OkilumManagerFocus *> *KeyRouters;
static NSMutableSet<NSString *> *KeyHooks;
static void (^KeyOriginalSendEvent)(NSEvent *);

static NSString *KeyAction(NSEvent *event) {
    if (event.type != NSEventTypeKeyDown) return nil;
    NSEventModifierFlags f = event.modifierFlags;
    const char *selector = okilum_manager_edit_action_for_keycode(event.charactersIgnoringModifiers.lowercaseString.UTF8String, event.keyCode,
        (f & NSEventModifierFlagCommand) != 0, (f & NSEventModifierFlagShift) != 0,
        (f & NSEventModifierFlagControl) != 0, (f & NSEventModifierFlagOption) != 0);
    return selector ? [NSString stringWithUTF8String:selector] : nil;
}
static OkilumManagerFocus *KeyFocusedRouter(void) {
    for (OkilumManagerFocus *router in KeyRouters) {
        [router refresh];
        if (router.focusCycle.armed && router.focused) return router;
    }
    return nil;
}
static NSString *KeyJSON(id value) {
    NSData *data = [NSJSONSerialization dataWithJSONObject:value options:NSJSONWritingFragmentsAllowed error:NULL];
    return data ? [[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding] : @"null";
}
static BOOL KeyOwnsMethod(Class cls, SEL selector) {
    unsigned int count = 0;
    Method *methods = class_copyMethodList(cls, &count);
    BOOL found = NO;
    for (unsigned int i = 0; i < count; i++) if (method_getName(methods[i]) == selector) found = YES;
    free(methods);
    return found;
}
static BOOL KeyReserveHook(Class cls, SEL selector) {
    NSString *key = [NSString stringWithFormat:@"%@/%@", NSStringFromClass(cls), NSStringFromSelector(selector)];
    if ([KeyHooks containsObject:key]) return NO;
    [KeyHooks addObject:key];
    return YES;
}
static void KeyHookEvent(Class cls, SEL selector, BOOL returnsBool, BOOL intercept) {
    if (!KeyOwnsMethod(cls, selector) || !KeyReserveHook(cls, selector)) return;
    Method method = class_getInstanceMethod(cls, selector);
    IMP original = method_getImplementation(method);
    IMP replacement;
    if (returnsBool) {
        replacement = imp_implementationWithBlock(^BOOL(id object, NSEvent *event) {
            OkilumManagerFocus *router = KeyFocusedRouter();
            if (intercept && router && !router.nativeRouting && [router handleCommandEvent:event]) {
                return YES;
            }
            BOOL result = ((BOOL (*)(id, SEL, NSEvent *))original)(object, selector, event);
            return result;
        });
    } else {
        replacement = imp_implementationWithBlock(^(id object, NSEvent *event) {
            ((void (*)(id, SEL, NSEvent *))original)(object, selector, event);
        });
    }
    class_replaceMethod(cls, selector, replacement, method_getTypeEncoding(method));
}
static void KeyHookChain(Class cls, SEL selector, BOOL returnsBool, BOOL intercept) {
    for (Class current = cls; current && current != NSObject.class; current = class_getSuperclass(current)) {
        KeyHookEvent(current, selector, returnsBool, intercept);
    }
}
// FIFO is shared by production dispatch and non-GUI native unit tests.
@interface OkilumCommandQueue : NSObject
@property(nonatomic, strong) NSMutableArray *pending;
@property(nonatomic) NSUInteger nextIdentifier;
@property(nonatomic) NSUInteger activeIdentifier;
- (void)submit:(void (^)(NSUInteger))work;
- (void)complete:(NSUInteger)identifier;
- (void)cancel;
- (void)drain;
@end
@implementation OkilumCommandQueue
- (void)submit:(void (^)(NSUInteger))work {
    if (!self.pending) self.pending = [NSMutableArray new];
    NSUInteger identifier = ++self.nextIdentifier;
    [self.pending addObject:@{@"id":@(identifier), @"work":[work copy]}];
    [self drain];
}
- (void)drain {
    if (self.activeIdentifier || !self.pending.count) return;
    NSDictionary *next = self.pending.firstObject;
    [self.pending removeObjectAtIndex:0];
    self.activeIdentifier = [next[@"id"] unsignedIntegerValue];
    void (^work)(NSUInteger) = next[@"work"];
    work(self.activeIdentifier);
}
- (void)complete:(NSUInteger)identifier {
    if (self.activeIdentifier != identifier) return;
    self.activeIdentifier = 0;
    [self drain];
}
- (void)cancel { self.activeIdentifier = 0; [self.pending removeAllObjects]; }
@end
static void KeyCancelCommands(id object) {
    OkilumManagerFocus *router = object;
    [router.commandQueue cancel];
    router.completedContext = nil;
}
static void KeyNoteUserInput(id object, NSEvent *event) {
    OkilumManagerFocus *router = object;
    if (router.nativeRouting) return;
    BOOL pointer = event.type == NSEventTypeLeftMouseDown || event.type == NSEventTypeRightMouseDown;
    BOOL typing = event.type == NSEventTypeKeyDown && !KeyAction(event);
    if (!pointer && !(typing && router.focused)) return;
    router.keyUserActivity++;
    KeyCancelCommands(router);
}
static void KeyFinish(OkilumManagerFocus *router, NSUInteger serial, NSDictionary *context) {
    if (router.commandQueue.activeIdentifier != serial) return;
    if (![context isKindOfClass:NSDictionary.class]) { KeyCancelCommands(router); return; }
    router.completedContext = context;
    [router.commandQueue complete:serial];
}
// Two commands must survive acceptance while the first native attempt has no
// effect. Completing an obsolete token must never release a newer command.
bool okilum_manager_command_queue_regression(bool undo) {
    @autoreleasepool {
        OkilumCommandQueue *queue = [OkilumCommandQueue new];
        NSMutableArray *order = [NSMutableArray new];
        __block NSUInteger first = 0;
        __block NSUInteger second = 0;
        [queue submit:^(NSUInteger token) { first = token; [order addObject:@"paste"]; }];
        [queue submit:^(NSUInteger token) { second = token; [order addObject:undo ? @"undo" : @"paste"]; }];
        if (order.count != 1 || !first || second) return false;
        [queue complete:first];
        if (![order isEqualToArray:@[@"paste", undo ? @"undo" : @"paste"]] || !second) return false;
        [queue complete:first];
        if (queue.activeIdentifier != second) return false;
        [queue complete:second];
        [queue submit:^(NSUInteger token) { first = token; }];
        [queue cancel];
        [queue submit:^(NSUInteger token) { second = token; }];
        [queue complete:first];
        return queue.activeIdentifier == second;
    }
}
@interface OkilumKeyMessages : NSObject <WKScriptMessageHandler>
@property(nonatomic, weak) OkilumManagerFocus *router;
@end
@implementation OkilumKeyMessages
- (void)userContentController:(WKUserContentController *)controller didReceiveScriptMessage:(WKScriptMessage *)message {
    (void)controller;
    OkilumManagerFocus *router = self.router;
    if (!router || message.webView != router.webview || !message.frameInfo.mainFrame || ![message.body isKindOfClass:NSDictionary.class]) return;
    NSDictionary *context = message.body;
    if ([context[@"kind"] isEqual:@"invalid"]) {
        router.pageContext = nil;
        router.keyDocumentGeneration++;
        KeyCancelCommands(router);
        return;
    }
    if (![context[@"doc"] isKindOfClass:NSString.class] || ![context[@"origin"] isKindOfClass:NSString.class]) return;
    if (![router.pageContext[@"doc"] isEqual:context[@"doc"]]) {
        router.keyDocumentGeneration++;
        KeyCancelCommands(router);
    }
    router.pageContext = context;
}
@end
static void KeyInstallDocumentTracker(OkilumManagerFocus *router) {
    WKUserContentController *controller = router.webview.configuration.userContentController;
    OkilumKeyMessages *messages = [OkilumKeyMessages new];
    messages.router = router;
    router.keyMessages = messages;
    [controller addScriptMessageHandler:messages name:@"okilumNativeKeysV2"];
    WKUserScript *script = [[WKUserScript alloc] initWithSource:KeyPageScript injectionTime:WKUserScriptInjectionTimeAtDocumentStart forMainFrameOnly:YES];
    [controller addUserScript:script];
    [router.webview evaluateJavaScript:KeyPageScript completionHandler:^(id value, NSError *error) {
        (void)value;
    }];
}
static BOOL KeyCurrent(OkilumManagerFocus *router, NSUInteger serial, NSUInteger generation, NSUInteger activity) {
    BOOL valid = router && router.focused && router.commandQueue.activeIdentifier == serial
        && router.keyDocumentGeneration == generation && router.keyUserActivity == activity;
    if (!valid && router.commandQueue.activeIdentifier == serial) KeyCancelCommands(router);
    return valid;
}
static void KeyInstallRouter(id object) {
    OkilumManagerFocus *router = object;
    if (!KeyRouters) KeyRouters = [NSHashTable weakObjectsHashTable];
    if (!KeyHooks) KeyHooks = [NSMutableSet new];
    [KeyRouters addObject:router];
    router.commandQueue = [OkilumCommandQueue new];
    KeyInstallDocumentTracker(router);
    Class appClass = NSApp.class;
    SEL sendEvent = @selector(sendEvent:);
    if (KeyReserveHook(appClass, sendEvent)) {
        Method method = class_getInstanceMethod(appClass, sendEvent);
        IMP original = method_getImplementation(method);
        KeyOriginalSendEvent = ^(NSEvent *event) { ((void (*)(id, SEL, NSEvent *))original)(NSApp, sendEvent, event); };
        IMP replacement = imp_implementationWithBlock(^(id app, NSEvent *event) {
            OkilumManagerFocus *focused = KeyFocusedRouter();
            if (focused && !focused.nativeRouting) KeyNoteUserInput(focused, event);
            if (focused && !focused.nativeRouting && [focused handleCommandEvent:event]) return;
            ((void (*)(id, SEL, NSEvent *))original)(app, sendEvent, event);
        });
        class_replaceMethod(appClass, sendEvent, replacement, method_getTypeEncoding(method));
    }
    SEL sendAction = @selector(sendAction:to:from:);
    if (KeyReserveHook(appClass, sendAction)) {
        Method method = class_getInstanceMethod(appClass, sendAction);
        IMP original = method_getImplementation(method);
        IMP replacement = imp_implementationWithBlock(^BOOL(id app, SEL action, id target, id sender) {
            OkilumManagerFocus *focused = KeyFocusedRouter();
            NSMenuItem *item = [sender isKindOfClass:NSMenuItem.class] ? sender : nil;
            if (focused && !focused.nativeRouting && item.keyEquivalent.length) {
                NSEvent *event = [NSEvent keyEventWithType:NSEventTypeKeyDown location:NSZeroPoint modifierFlags:item.keyEquivalentModifierMask
                    timestamp:NSProcessInfo.processInfo.systemUptime windowNumber:focused.webview.window.windowNumber context:nil
                    characters:item.keyEquivalent charactersIgnoringModifiers:item.keyEquivalent isARepeat:NO keyCode:0];
                if ([focused handleCommandEvent:event]) { return YES; }
            }
            BOOL result = ((BOOL (*)(id, SEL, SEL, id, id))original)(app, sendAction, action, target, sender);
            return result;
        });
        class_replaceMethod(appClass, sendAction, replacement, method_getTypeEncoding(method));
    }
    // Window hooks also intercept commands invoked outside NSApp.sendEvent.
    KeyHookChain(router.webview.window.class, @selector(performKeyEquivalent:), YES, YES);

}
static void KeyRemoveRouter(id object) {
    OkilumManagerFocus *router = object;
    [router.webview.configuration.userContentController removeScriptMessageHandlerForName:@"okilumNativeKeysV2"];
    router.keyMessages = nil;
    [KeyRouters removeObject:router];
}

@implementation OkilumManagerFocus (Keys)
- (BOOL)handleCommandEvent:(NSEvent *)event {
    NSString *action = KeyAction(event);
    if (!action || self.nativeRouting || !self.focused || (event.window && event.window != self.webview.window)) return NO;
    [self refresh];
    if (!self.focusCycle.armed) return NO;
    BOOL deferred = self.commandQueue.activeIdentifier != 0;
    NSUInteger generation = self.keyDocumentGeneration;
    NSUInteger activity = self.keyUserActivity;
    NSDictionary *original = self.pageContext;
    __weak OkilumManagerFocus *weak = self;
    [self.commandQueue submit:^(NSUInteger serial) {
        OkilumManagerFocus *router = weak;
        if (!KeyCurrent(router, serial, generation, activity)) { KeyCancelCommands(router); return; }
        if (!deferred) {
            // The first command keeps synchronous native delivery on its event path.
            [router performNativeCommand:event before:original serial:serial generation:generation activity:activity];
            return;
        }
        // Commands accepted while an earlier fallback is pending keep FIFO order.
        // Rebase only over the completed command's own edits. A user key/click,
        // programmatic focus change, navigation or foreign edit cancels the queue.
        NSString *guard = [NSString stringWithFormat:@"window.__okilumKeysV2?.rebase(%@,%@) ?? {cancel:true,reason:'missing-document-state'}", KeyJSON(original ?: NSNull.null), KeyJSON(router.completedContext ?: NSNull.null)];
        [router.webview evaluateJavaScript:guard completionHandler:^(id result, NSError *error) {
            OkilumManagerFocus *current = weak;
            if (!KeyCurrent(current, serial, generation, activity)) return;
            NSDictionary *context = [result isKindOfClass:NSDictionary.class] ? result[@"context"] : nil;
            if (error || ![context isKindOfClass:NSDictionary.class] || [result[@"cancel"] boolValue]) {
                KeyCancelCommands(current); return;
            }
            [current performNativeCommand:event before:context serial:serial generation:generation activity:activity];
        }];
    }];
    return YES;
}
- (void)performNativeCommand:(NSEvent *)event before:(NSDictionary *)before serial:(NSUInteger)serial generation:(NSUInteger)generation activity:(NSUInteger)activity {
    if (!KeyCurrent(self, serial, generation, activity)) return;
    NSString *action = KeyAction(event);
    NSInteger clipboardBefore = NSPasteboard.generalPasteboard.changeCount;
    self.nativeRouting = YES;
    @try {
        if (KeyOriginalSendEvent) {
            KeyOriginalSendEvent(event);
        } else { [self dispatchEditAction:NSSelectorFromString(action) sender:self]; }
    } @finally { self.nativeRouting = NO; }
    if (!before) {  KeyCancelCommands(self); return; }
    __weak OkilumManagerFocus *weak = self;
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 80 * NSEC_PER_MSEC), dispatch_get_main_queue(), ^{
        OkilumManagerFocus *router = weak;
        if (!KeyCurrent(router, serial, generation, activity)) return;
        NSString *observe = [NSString stringWithFormat:@"window.__okilumKeysV2?.observe(%@,%@) ?? {cancel:true,reason:'missing-document-state'}", KeyJSON(before), KeyJSON(action)];
        [router.webview evaluateJavaScript:observe completionHandler:^(id result, NSError *error) {
            OkilumManagerFocus *current = weak;
            if (!KeyCurrent(current, serial, generation, activity)) return;
            if (error || ![result isKindOfClass:NSDictionary.class] || [result[@"cancel"] boolValue]) {
                KeyCancelCommands(current); return;
            }
            BOOL native = [result[@"native"] boolValue];
            if ([action isEqualToString:@"copy:"] && NSPasteboard.generalPasteboard.changeCount != clipboardBefore) native = YES;
            NSDictionary *context = result[@"context"];
            if (![context isKindOfClass:NSDictionary.class]) { KeyCancelCommands(current); return; }
            if (native) {  KeyFinish(current, serial, context); return; }
            [current pageCommand:action context:context serial:serial generation:generation activity:activity];
        }];
    });
}
- (void)pageCommand:(NSString *)action context:(NSDictionary *)context serial:(NSUInteger)serial generation:(NSUInteger)generation activity:(NSUInteger)activity {
    if (!KeyCurrent(self, serial, generation, activity)) return;
    NSDictionary *payload = nil;
    if ([action isEqualToString:@"paste:"]) {
        NSPasteboard *clipboard = NSPasteboard.generalPasteboard;
        NSData *image = [clipboard dataForType:NSPasteboardTypePNG];
        if (!image) {
            NSData *tiff = [clipboard dataForType:NSPasteboardTypeTIFF];
            NSBitmapImageRep *bitmap = tiff ? [NSBitmapImageRep imageRepWithData:tiff] : nil;
            image = [bitmap representationUsingType:NSBitmapImageFileTypePNG properties:@{}];
        }
        if (image) {
            payload = @{@"kind":@"image", @"data":[image base64EncodedStringWithOptions:0]};
        } else {
            NSString *text = [clipboard stringForType:NSPasteboardTypeString];
            if (!text) {  KeyFinish(self, serial, context); return; }
            payload = @{@"kind":@"text", @"data":text};
        }
    }
    NSString *script = [NSString stringWithFormat:@"window.__okilumKeysV2?.run(%@,%@,%@) ?? {ok:false,reason:'missing-document-state'}", KeyJSON(action), KeyJSON(payload ?: NSNull.null), KeyJSON(context)];
    __weak OkilumManagerFocus *weak = self;
    [self.webview evaluateJavaScript:script completionHandler:^(id result, NSError *error) {
        OkilumManagerFocus *router = weak;
        if (!KeyCurrent(router, serial, generation, activity)) return;
        BOOL valid = !error && [result isKindOfClass:NSDictionary.class] && [result[@"ok"] boolValue];
        NSDictionary *completed = [result isKindOfClass:NSDictionary.class] ? result[@"context"] : nil;
        if (!valid || !([action isEqualToString:@"copy:"] || [action isEqualToString:@"cut:"])) { KeyFinish(router, serial, completed); return; }
        NSString *text = result[@"text"];
        if (![text isKindOfClass:NSString.class] || text.length == 0) { KeyFinish(router, serial, completed); return; }
        [NSPasteboard.generalPasteboard clearContents];
        BOOL written = [NSPasteboard.generalPasteboard setString:text forType:NSPasteboardTypeString];
        if (written && [action isEqualToString:@"cut:"]) {
            NSString *deletion = [NSString stringWithFormat:@"window.__okilumKeysV2?.run('delete:',null,%@) ?? {ok:false,reason:'missing-document-state'}", KeyJSON(context)];
            [router.webview evaluateJavaScript:deletion completionHandler:^(id value, NSError *failure) {
                OkilumManagerFocus *last = weak;
                if (!KeyCurrent(last, serial, generation, activity)) return;
                KeyFinish(last, serial, [value isKindOfClass:NSDictionary.class] ? value[@"context"] : nil);
            }];
            return;
        }
        KeyFinish(router, serial, completed);
    }];
}
@end

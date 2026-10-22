// App-owned instrumentation of public AppKit entry points. No GPUI/Wry source
// patch, private WebKit selector or clipboard JavaScript permission bypass.
#include "page_keys.inc"

static NSHashTable<OkilumManagerFocus *> *KeyRouters;
static NSMutableSet<NSString *> *KeyHooks;
static void (^KeyOriginalSendEvent)(NSEvent *);

static BOOL KeyDiagnostics(void) {
    return [NSProcessInfo.processInfo.environment[@"OKILUM_SPRINT_KEYS"] isEqualToString:@"1"];
}
static void KeyLog(NSString *format, ...) {
    if (!KeyDiagnostics()) return;
    va_list args;
    va_start(args, format);
    NSString *message = [[NSString alloc] initWithFormat:format arguments:args];
    va_end(args);
    // Never log clipboard text, page values, selection text or pairing tokens.
    NSString *directory = [NSHomeDirectory() stringByAppendingPathComponent:@"Library/Logs"];
    [NSFileManager.defaultManager createDirectoryAtPath:directory withIntermediateDirectories:YES attributes:nil error:NULL];
    NSString *path = [directory stringByAppendingPathComponent:@"okilum-sprint-keys.log"];
    @synchronized (KeyRouters) {
        if (![NSFileManager.defaultManager fileExistsAtPath:path]) {
            [NSFileManager.defaultManager createFileAtPath:path contents:nil attributes:@{NSFilePosixPermissions: @0600}];
        }
        NSFileHandle *file = [NSFileHandle fileHandleForWritingAtPath:path];
        [file seekToEndOfFile];
        [file writeData:[[NSString stringWithFormat:@"%@ %@\n", NSDate.date, message] dataUsingEncoding:NSUTF8StringEncoding]];
        [file closeFile];
    }
}
static NSString *KeyAction(NSEvent *event) {
    if (event.type != NSEventTypeKeyDown) return nil;
    NSEventModifierFlags f = event.modifierFlags;
    const char *selector = okilum_manager_edit_action(event.charactersIgnoringModifiers.lowercaseString.UTF8String,
        (f & NSEventModifierFlagCommand) != 0, (f & NSEventModifierFlagShift) != 0,
        (f & NSEventModifierFlagControl) != 0, (f & NSEventModifierFlagOption) != 0);
    return selector ? [NSString stringWithUTF8String:selector] : nil;
}
static OkilumManagerFocus *KeyFocusedRouter(void) {
    for (OkilumManagerFocus *router in KeyRouters) if (router.focused) return router;
    return nil;
}
static NSString *KeyJSON(id value) {
    NSData *data = [NSJSONSerialization dataWithJSONObject:value options:NSJSONWritingFragmentsAllowed error:NULL];
    return data ? [[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding] : @"null";
}
static void KeyMenuHit(NSMenu *menu, NSEvent *event) {
    NSEventModifierFlags mask = NSEventModifierFlagCommand | NSEventModifierFlagShift | NSEventModifierFlagControl | NSEventModifierFlagOption;
    for (NSMenuItem *item in menu.itemArray) {
        if (item.submenu) KeyMenuHit(item.submenu, event);
        if (item.keyEquivalent.length && [item.keyEquivalent.lowercaseString isEqualToString:event.charactersIgnoringModifiers.lowercaseString]
            && (item.keyEquivalentModifierMask & mask) == (event.modifierFlags & mask)) {
            KeyLog(@"menu candidate title=%@ action=%@ target=%@ enabled=%d", item.title, NSStringFromSelector(item.action), NSStringFromClass([item.target class]), item.enabled);
        }
    }
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
    NSString *name = NSStringFromClass(cls);
    IMP replacement;
    if (returnsBool) {
        replacement = imp_implementationWithBlock(^BOOL(id object, NSEvent *event) {
            BOOL command = (event.modifierFlags & NSEventModifierFlagCommand) != 0;
            OkilumManagerFocus *router = KeyFocusedRouter();
            if (command) KeyLog(@"%@.%@ enter firstResponder=%@ nativeRouting=%d", name, NSStringFromSelector(selector), NSStringFromClass(router.webview.window.firstResponder.class), router.nativeRouting);
            if (intercept && router && !router.nativeRouting && [router handleCommandEvent:event]) {
                KeyLog(@"%@.%@ returns YES (browser command queued)", name, NSStringFromSelector(selector));
                return YES;
            }
            BOOL result = ((BOOL (*)(id, SEL, NSEvent *))original)(object, selector, event);
            if (command) KeyLog(@"%@.%@ returns %@", name, NSStringFromSelector(selector), result ? @"YES" : @"NO");
            return result;
        });
    } else {
        replacement = imp_implementationWithBlock(^(id object, NSEvent *event) {
            BOOL command = (event.modifierFlags & NSEventModifierFlagCommand) != 0;
            if (command) KeyLog(@"%@.%@ called receiver=%@", name, NSStringFromSelector(selector), NSStringFromClass([object class]));
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
        KeyLog(@"document invalidated; pending page fallback cancelled");
        return;
    }
    if (![context[@"doc"] isKindOfClass:NSString.class] || ![context[@"origin"] isKindOfClass:NSString.class]) return;
    if (![router.pageContext[@"doc"] isEqual:context[@"doc"]]) router.keyDocumentGeneration++;
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
        KeyLog(@"page context tracker installed errorCode=%ld", (long)error.code);
    }];
}
static BOOL KeyCurrent(OkilumManagerFocus *router, NSUInteger serial, NSUInteger generation) {
    return router && router.focused && router.keySerial == serial && router.keyDocumentGeneration == generation;
}
static void KeyInstallRouter(id object) {
    OkilumManagerFocus *router = object;
    if (!KeyRouters) KeyRouters = [NSHashTable weakObjectsHashTable];
    if (!KeyHooks) KeyHooks = [NSMutableSet new];
    [KeyRouters addObject:router];
    KeyInstallDocumentTracker(router);
    Class appClass = NSApp.class;
    SEL sendEvent = @selector(sendEvent:);
    if (KeyReserveHook(appClass, sendEvent)) {
        Method method = class_getInstanceMethod(appClass, sendEvent);
        IMP original = method_getImplementation(method);
        KeyOriginalSendEvent = ^(NSEvent *event) { ((void (*)(id, SEL, NSEvent *))original)(NSApp, sendEvent, event); };
        IMP replacement = imp_implementationWithBlock(^(id app, NSEvent *event) {
            OkilumManagerFocus *focused = KeyFocusedRouter();
            if (event.type == NSEventTypeKeyDown && (event.modifierFlags & NSEventModifierFlagCommand)) {
                KeyLog(@"NSApp.sendEvent key=%@ flags=%lu window=%ld firstResponder=%@ focused=%d nativeRouting=%d", event.charactersIgnoringModifiers, (unsigned long)event.modifierFlags, (long)event.windowNumber, NSStringFromClass(NSApp.keyWindow.firstResponder.class), focused != nil, focused.nativeRouting);
                KeyMenuHit(NSApp.mainMenu, event);
            }
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
            id resolved = target ?: [NSApp targetForAction:action to:nil from:sender];
            KeyLog(@"NSApp.sendAction action=%@ target=%@ resolvedTarget=%@ menuTitle=%@ firstResponder=%@ focused=%d nativeRouting=%d", NSStringFromSelector(action), NSStringFromClass([target class]), NSStringFromClass([resolved class]), item.title, NSStringFromClass(NSApp.keyWindow.firstResponder.class), focused != nil, focused.nativeRouting);
            if (focused && !focused.nativeRouting && item.keyEquivalent.length) {
                NSEvent *event = [NSEvent keyEventWithType:NSEventTypeKeyDown location:NSZeroPoint modifierFlags:item.keyEquivalentModifierMask
                    timestamp:NSProcessInfo.processInfo.systemUptime windowNumber:focused.webview.window.windowNumber context:nil
                    characters:item.keyEquivalent charactersIgnoringModifiers:item.keyEquivalent isARepeat:NO keyCode:0];
                if ([focused handleCommandEvent:event]) { KeyLog(@"NSApp.sendAction menu queued page-aware dispatch result=YES"); return YES; }
            }
            BOOL result = ((BOOL (*)(id, SEL, SEL, id, id))original)(app, sendAction, action, target, sender);
            KeyLog(@"NSApp.sendAction result=%@", result ? @"YES" : @"NO");
            return result;
        });
        class_replaceMethod(appClass, sendAction, replacement, method_getTypeEncoding(method));
    }
    // Window hooks also intercept commands invoked outside NSApp.sendEvent.
    KeyHookChain(router.webview.window.class, @selector(performKeyEquivalent:), YES, YES);
    if (KeyDiagnostics()) {
        KeyHookChain(router.parent.class, @selector(performKeyEquivalent:), YES, NO);
        KeyHookChain(router.webview.class, @selector(performKeyEquivalent:), YES, NO);
        KeyHookChain(router.webview.class, @selector(keyDown:), NO, NO);
        KeyLog(@"router attached bundle=%@ webview=%@ parent=%@ window=%@ log=~/Library/Logs/okilum-sprint-keys.log", NSBundle.mainBundle.bundleIdentifier, NSStringFromClass(router.webview.class), NSStringFromClass(router.parent.class), NSStringFromClass(router.webview.window.class));
    }
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
    NSUInteger serial = ++self.keySerial;
    NSUInteger generation = self.keyDocumentGeneration;
    NSDictionary *before = self.pageContext;
    NSInteger clipboardBefore = NSPasteboard.generalPasteboard.changeCount;
    KeyLog(@"command action=%@ serial=%lu responder=%@ contextReady=%d", action, (unsigned long)serial, NSStringFromClass(self.webview.window.firstResponder.class), before != nil);
    // Native delivery is synchronous on the original event path. Never replay
    // a cut/paste later, after the user has moved to another field or typed.
    self.nativeRouting = YES;
    @try {
        if (KeyOriginalSendEvent) {
            KeyLog(@"native synchronous NSApp original sendEvent action=%@", action);
            KeyOriginalSendEvent(event);
        } else {
            [self dispatchEditAction:NSSelectorFromString(action) sender:self];
        }
    } @finally { self.nativeRouting = NO; }
    if (!before) { KeyLog(@"fallback cancelled action=%@ reason=context-not-ready", action); return YES; }
    __weak OkilumManagerFocus *weak = self;
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 80 * NSEC_PER_MSEC), dispatch_get_main_queue(), ^{
        OkilumManagerFocus *router = weak;
        if (!KeyCurrent(router, serial, generation)) { KeyLog(@"fallback cancelled action=%@ reason=new-command-document-or-native-focus", action); return; }
        NSString *observe = [NSString stringWithFormat:@"window.__okilumKeysV2?.observe(%@,%@) ?? {cancel:true,reason:'missing-document-state'}", KeyJSON(before), KeyJSON(action)];
        [router.webview evaluateJavaScript:observe completionHandler:^(id result, NSError *error) {
            OkilumManagerFocus *current = weak;
            if (!KeyCurrent(current, serial, generation)) return;
            if (error || ![result isKindOfClass:NSDictionary.class]) { KeyLog(@"fallback cancelled action=%@ reason=observation-error code=%ld", action, (long)error.code); return; }
            if ([result[@"cancel"] boolValue]) { KeyLog(@"fallback cancelled action=%@ reason=%@", action, result[@"reason"]); return; }
            BOOL native = [result[@"native"] boolValue];
            if ([action isEqualToString:@"copy:"] && NSPasteboard.generalPasteboard.changeCount != clipboardBefore) native = YES;
            if (native) { KeyLog(@"path=NATIVE action=%@ fallback=NOT_USED", action); return; }
            NSDictionary *context = result[@"context"];
            if (![context isKindOfClass:NSDictionary.class]) { KeyLog(@"fallback cancelled reason=missing-matching-context"); return; }
            [current pageCommand:action context:context serial:serial generation:generation];
        }];
    });
    return YES;
}
- (void)pageCommand:(NSString *)action context:(NSDictionary *)context serial:(NSUInteger)serial generation:(NSUInteger)generation {
    if (!KeyCurrent(self, serial, generation)) return;
    KeyLog(@"path=PAGE_FALLBACK action=%@", action);
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
            KeyLog(@"page paste kind=image bytes=%lu", (unsigned long)image.length);
        } else {
            NSString *text = [clipboard stringForType:NSPasteboardTypeString];
            if (!text) { KeyLog(@"page paste skipped no-supported-clipboard-data"); return; }
            payload = @{@"kind":@"text", @"data":text};
            KeyLog(@"page paste kind=text length=%lu", (unsigned long)text.length);
        }
    }
    NSString *script = [NSString stringWithFormat:@"window.__okilumKeysV2?.run(%@,%@,%@) ?? {ok:false,reason:'missing-document-state'}", KeyJSON(action), KeyJSON(payload ?: NSNull.null), KeyJSON(context)];
    __weak OkilumManagerFocus *weak = self;
    [self.webview evaluateJavaScript:script completionHandler:^(id result, NSError *error) {
        OkilumManagerFocus *router = weak;
        if (!KeyCurrent(router, serial, generation)) return;
        BOOL valid = !error && [result isKindOfClass:NSDictionary.class] && [result[@"ok"] boolValue];
        KeyLog(@"page fallback completed action=%@ ok=%d reason=%@ errorCode=%ld", action, valid, [result isKindOfClass:NSDictionary.class] ? result[@"reason"] : nil, (long)error.code);
        if (!valid || !([action isEqualToString:@"copy:"] || [action isEqualToString:@"cut:"])) return;
        NSString *text = result[@"text"];
        if (![text isKindOfClass:NSString.class] || text.length == 0) return;
        [NSPasteboard.generalPasteboard clearContents];
        BOOL written = [NSPasteboard.generalPasteboard setString:text forType:NSPasteboardTypeString];
        KeyLog(@"page copy clipboardWritten=%d length=%lu", written, (unsigned long)text.length);
        if (written && [action isEqualToString:@"cut:"]) {
            NSString *deletion = [NSString stringWithFormat:@"window.__okilumKeysV2?.run('delete:',null,%@) ?? {ok:false,reason:'missing-document-state'}", KeyJSON(context)];
            [router.webview evaluateJavaScript:deletion completionHandler:^(id value, NSError *failure) {
                KeyLog(@"page cut completed ok=%@ reason=%@ errorCode=%ld", [value isKindOfClass:NSDictionary.class] ? value[@"ok"] : nil, [value isKindOfClass:NSDictionary.class] ? value[@"reason"] : nil, (long)failure.code);
            }];
        }
    }];
}
@end

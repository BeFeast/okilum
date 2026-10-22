// App-owned instrumentation of public AppKit entry points. No GPUI/Wry source
// patch, private WebKit selector or clipboard JavaScript permission bypass.
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
static void KeyInstallRouter(id object) {
    OkilumManagerFocus *router = object;
    if (!KeyRouters) KeyRouters = [NSHashTable weakObjectsHashTable];
    if (!KeyHooks) KeyHooks = [NSMutableSet new];
    [KeyRouters addObject:router];
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
static void KeyRemoveRouter(id router) { [KeyRouters removeObject:router]; }

@implementation OkilumManagerFocus (Keys)
- (BOOL)handleCommandEvent:(NSEvent *)event {
    NSString *action = KeyAction(event);
    if (!action || self.nativeRouting || !self.focused) return NO;
    KeyLog(@"browser command queued action=%@ responder=%@", action, NSStringFromClass(self.webview.window.firstResponder.class));
    if (!self.keyQueue) self.keyQueue = [NSMutableArray new];
    [self.keyQueue addObject:@{@"action":action, @"event":event}];
    if (!self.keyBusy) [self processKeyQueue];
    return YES;
}
- (void)finishKeyCommand {
    if (self.keyToken) {
        NSString *cleanup = [NSString stringWithFormat:@"(()=>{const t=%@;const s=window.__okilumNativeKeys_v1?.[t];if(s){s.types.forEach(x=>document.removeEventListener(x,s.h,true));delete window.__okilumNativeKeys_v1[t]}})()", KeyJSON(self.keyToken)];
        [self.webview evaluateJavaScript:cleanup completionHandler:nil];
        self.keyToken = nil;
    }
    self.keyBusy = NO;
    if (self.keyQueue.count) [self processKeyQueue];
}
- (void)processKeyQueue {
    if (!self.keyQueue.count) return;
    if (!self.focused) { [self.keyQueue removeAllObjects]; return; }
    self.keyBusy = YES;
    NSDictionary *command = self.keyQueue.firstObject;
    [self.keyQueue removeObjectAtIndex:0];
    NSString *action = command[@"action"];
    NSString *token = NSUUID.UUID.UUIDString;
    self.keyToken = token;
    NSString *prepare = [NSString stringWithFormat:
        @"(()=>{const t=%@;let a=document.activeElement;while(a&&a.shadowRoot&&a.shadowRoot.activeElement)a=a.shadowRoot.activeElement;"
        "const snap=()=>JSON.stringify([a&&('value'in a?a.value:a.innerHTML),a&&a.selectionStart,a&&a.selectionEnd,String(window.getSelection())]);"
        "const s={a,before:snap(),snap,events:[]};s.h=e=>{if(e.isTrusted)s.events.push(e.type)};"
        "s.types=['paste','copy','cut','beforeinput','input'];s.types.forEach(x=>document.addEventListener(x,s.h,true));"
        "(window.__okilumNativeKeys_v1??={})[t]=s;return {tag:a&&a.tagName,editable:!!(a&&a.isContentEditable)}})()", KeyJSON(token)];
    __weak OkilumManagerFocus *weak = self;
    [self.webview evaluateJavaScript:prepare completionHandler:^(id info, NSError *error) {
        OkilumManagerFocus *router = weak;
        if (!router || !router.focused) { [router finishKeyCommand]; return; }
        KeyLog(@"page probe action=%@ prepared=%d tag=%@", action, error == nil, [info isKindOfClass:NSDictionary.class] ? info[@"tag"] : nil);
        NSInteger clipboardBefore = NSPasteboard.generalPasteboard.changeCount;
        router.nativeRouting = YES;
        if (KeyOriginalSendEvent) {
            KeyLog(@"native attempt NSApp original sendEvent action=%@", action);
            KeyOriginalSendEvent(command[@"event"]);
        } else {
            [router dispatchEditAction:NSSelectorFromString(action) sender:router];
        }
        router.nativeRouting = NO;
        dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 120 * NSEC_PER_MSEC), dispatch_get_main_queue(), ^{
            OkilumManagerFocus *current = weak;
            if (!current || !current.focused) { [current finishKeyCommand]; return; }
            NSString *check = [NSString stringWithFormat:
                @"(()=>{const s=window.__okilumNativeKeys_v1?.[%@];if(!s)return {delivered:false};"
                "s.types.forEach(x=>document.removeEventListener(x,s.h,true));"
                "return {delivered:s.events.length>0||s.before!==s.snap(),events:s.events.length,sameTarget:s.a===document.activeElement}})()", KeyJSON(token)];
            [current.webview evaluateJavaScript:check completionHandler:^(id result, NSError *failure) {
                OkilumManagerFocus *last = weak;
                if (!last || !last.focused) { [last finishKeyCommand]; return; }
                BOOL delivered = !failure && [result isKindOfClass:NSDictionary.class] && [result[@"delivered"] boolValue];
                if ([action isEqualToString:@"copy:"] && NSPasteboard.generalPasteboard.changeCount != clipboardBefore) delivered = YES;
                KeyLog(@"native observation action=%@ delivered=%d trustedEvents=%@ errorCode=%ld", action, delivered, [result isKindOfClass:NSDictionary.class] ? result[@"events"] : nil, (long)failure.code);
                if (delivered) {
                    [last.webview evaluateJavaScript:[NSString stringWithFormat:@"delete window.__okilumNativeKeys_v1?.[%@]", KeyJSON(token)] completionHandler:nil];
                    KeyLog(@"path=NATIVE action=%@ fallback=NOT_USED", action);
                    [last finishKeyCommand];
                } else {
                    [last pageCommand:action token:token];
                }
            }];
        });
    }];
}
- (void)pageCommand:(NSString *)action token:(NSString *)token {
    if (!self.focused) { [self finishKeyCommand]; return; }
    KeyLog(@"path=PAGE_FALLBACK action=%@", action);
    NSString *guard = [NSString stringWithFormat:
        @"let a=document.activeElement;while(a&&a.shadowRoot&&a.shadowRoot.activeElement)a=a.shadowRoot.activeElement;"
        "const s=window.__okilumNativeKeys_v1?.[%@];if(s&&s.a!==a)return {ok:false,reason:'focus-changed'};", KeyJSON(token)];
    NSString *operation;
    if ([action isEqualToString:@"paste:"]) {
        NSPasteboard *clipboard = NSPasteboard.generalPasteboard;
        NSData *image = [clipboard dataForType:NSPasteboardTypePNG];
        if (!image) {
            NSData *tiff = [clipboard dataForType:NSPasteboardTypeTIFF];
            NSBitmapImageRep *bitmap = tiff ? [NSBitmapImageRep imageRepWithData:tiff] : nil;
            image = [bitmap representationUsingType:NSBitmapImageFileTypePNG properties:@{}];
        }
        if (image) {
            NSString *encoded = [image base64EncodedStringWithOptions:0];
            KeyLog(@"page paste kind=image bytes=%lu", (unsigned long)image.length);
            operation = [NSString stringWithFormat:@"const bytes=Uint8Array.from(atob(%@),c=>c.charCodeAt(0));const dt=new DataTransfer();dt.items.add(new File([bytes],'clipboard.png',{type:'image/png'}));const e=new ClipboardEvent('paste',{clipboardData:dt,bubbles:true,cancelable:true});a.dispatchEvent(e);return {ok:true,kind:'image'};", KeyJSON(encoded)];
        } else {
            NSString *text = [clipboard stringForType:NSPasteboardTypeString];
            if (!text) { KeyLog(@"page paste skipped: no supported clipboard text/image"); [self finishKeyCommand]; return; }
            KeyLog(@"page paste kind=text available=%d length=%lu", text != nil, (unsigned long)text.length);
            operation = [NSString stringWithFormat:@"return {ok:document.execCommand('insertText',false,%@),kind:'text'};", KeyJSON(text ?: @"")];
        }
    } else if ([action isEqualToString:@"copy:"] || [action isEqualToString:@"cut:"]) {
        NSString *selection = [NSString stringWithFormat:@"(()=>{%@return {text:typeof a?.selectionStart==='number'?a.value.slice(a.selectionStart,a.selectionEnd):String(window.getSelection())}})()", guard];
        __weak OkilumManagerFocus *weak = self;
        [self.webview evaluateJavaScript:selection completionHandler:^(id result, NSError *error) {
            OkilumManagerFocus *router = weak;
            if (!router || !router.focused) { [router finishKeyCommand]; return; }
            NSString *text = [result isKindOfClass:NSDictionary.class] ? result[@"text"] : nil;
            if (!error && text.length) {
                [NSPasteboard.generalPasteboard clearContents];
                BOOL written = [NSPasteboard.generalPasteboard setString:text forType:NSPasteboardTypeString];
                KeyLog(@"page copy clipboardWritten=%d length=%lu", written, (unsigned long)text.length);
                if (written && [action isEqualToString:@"cut:"]) {
                    [router.webview evaluateJavaScript:[NSString stringWithFormat:@"(()=>{%@return {ok:document.execCommand('delete')}})()", guard] completionHandler:^(id ignored, NSError *failure) {
                        (void)ignored; KeyLog(@"page cut completed errorCode=%ld", (long)failure.code); [weak finishKeyCommand];
                    }];
                    return;
                }
            } else { KeyLog(@"page copy skipped emptySelection=%d errorCode=%ld", text != nil && text.length == 0, (long)error.code); }
            [router finishKeyCommand];
        }];
        return;
    } else {
        NSString *name = [action isEqualToString:@"selectAll:"] ? @"selectAll" : ([action isEqualToString:@"redo:"] ? @"redo" : @"undo");
        operation = [NSString stringWithFormat:@"return {ok:document.execCommand(%@)};", KeyJSON(name)];
    }
    NSString *script = [NSString stringWithFormat:@"(()=>{%@%@})()", guard, operation];
    __weak OkilumManagerFocus *weak = self;
    [self.webview evaluateJavaScript:script completionHandler:^(id result, NSError *error) {
        KeyLog(@"page fallback completed action=%@ ok=%@ errorCode=%ld", action, [result isKindOfClass:NSDictionary.class] ? result[@"ok"] : nil, (long)error.code);
        [weak finishKeyCommand];
    }];
}
@end

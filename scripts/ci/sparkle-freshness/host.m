// Hosted-only integration host. The production driver and real Sparkle own
// version selection, network, archive validation, extraction and installation.
// These overrides automate About Install/Restart choices; they do not select a
// version or fabricate a Sparkle state/reply.
#import <objc/runtime.h>
#import "../../../crates/okilum-shell/src/updater/bridge.m"

static void Trace(NSString *event) {
    NSString *path = [NSBundle.mainBundle objectForInfoDictionaryKey:@"TSTrace"];
    NSFileHandle *file = [NSFileHandle fileHandleForWritingAtPath:path];
    [file seekToEndOfFile];
    [file writeData:[[event stringByAppendingString:@"\n"] dataUsingEncoding:NSUTF8StringEncoding]];
    [file closeFile];
}

static BOOL RotatesGroup(void) {
    return [[NSBundle.mainBundle objectForInfoDictionaryKey:@"TSRotateGroupAfterDownload"] boolValue];
}

// Read-only instrumentation of the pinned framework's EXISTING defaults object.
// A second defaults instance or CFPreferences receipt cannot prove what SUHost
// actually sees. Never replace a Sparkle method, host, defaults object or value
// through this observer. Missing layout is a fixture failure, not reproduction.
static BOOL TSObserverIncomplete;
static NSDictionary *GroupSnapshot(SPUUpdater *updater, NSString *role, NSString *boundary) {
    Ivar hostSlot = class_getInstanceVariable(SPUUpdater.class, "_host");
    id host = hostSlot && updater ? object_getIvar(updater, hostSlot) : nil;
    Ivar defaultsSlot = host ? class_getInstanceVariable([host class], "_userDefaults") : NULL;
    id defaults = defaultsSlot ? object_getIvar(host, defaultsSlot) : nil;
    BOOL validDefaults = [defaults isKindOfClass:NSUserDefaults.class];
    id group = validDefaults ? [defaults objectForKey:@"SUUpdateGroupIdentifier"] : nil;
    BOOL number = [group isKindOfClass:NSNumber.class];
    NSString *state = !updater ? @"updater-missing" : !hostSlot ? @"host-ivar-missing"
        : !host ? @"host-missing" : !defaultsSlot ? @"defaults-ivar-missing"
        : !defaults ? @"defaults-missing" : !validDefaults ? @"defaults-wrong-type"
        : !group ? @"group-missing" : !number ? @"group-wrong-type" : @"complete";
    if (![state isEqualToString:@"complete"]) TSObserverIncomplete = YES;
    NSDictionary *snapshot = @{
        @"role": role, @"boundary": boundary, @"state": state,
        @"updaterPresent": @((BOOL)(updater != nil)), @"hostIvarPresent": @((BOOL)(hostSlot != NULL)),
        @"hostPresent": @((BOOL)(host != nil)), @"defaultsIvarPresent": @((BOOL)(defaultsSlot != NULL)),
        @"defaultsPresent": @((BOOL)(defaults != nil)), @"defaultsIsNSUserDefaults": @(validDefaults),
        @"defaultsClass": defaults ? NSStringFromClass([defaults class]) : @"unavailable",
        @"groupType": number ? @"NSNumber" : group ? NSStringFromClass([group class]) : @"unavailable",
        @"group": number ? group : @"unavailable",
        @"domain": [updater.hostBundle objectForInfoDictionaryKey:@"SUDefaultsDomain"]
            ?: (updater.hostBundle == NSBundle.mainBundle ? @"standard-main-bundle"
                : updater.hostBundle.bundleIdentifier ?: @"unavailable")
    };
    NSData *json = [NSJSONSerialization dataWithJSONObject:snapshot options:0 error:NULL];
    Trace([@"rotation observation " stringByAppendingString:[[NSString alloc] initWithData:json encoding:NSUTF8StringEncoding]]);
    return snapshot;
}
static NSNumber *SnapshotNumber(NSDictionary *snapshot) {
    return [snapshot[@"state"] isEqualToString:@"complete"] ? snapshot[@"group"] : nil;
}
static NSString *GroupText(NSNumber *group) { return group ? group.stringValue : @"unavailable"; }

static NSNumber *LastProbeGroup;
static NSString *LastProbeSelection;
@interface TestFreshnessProbe : TSFreshnessProbe
@end
@implementation TestFreshnessProbe
- (void)updater:(SPUUpdater *)updater didFindValidUpdate:(SUAppcastItem *)item {
    GroupSnapshot(updater, @"probe", @"selection");
    Trace([@"rotation probe selection=" stringByAppendingString:item.versionString]);
    [super updater:updater didFindValidUpdate:item];
}
- (void)updater:(SPUUpdater *)updater didFinishUpdateCycleForUpdateCheck:(SPUUpdateCheck)check error:(NSError *)error {
    // Observe the real callback argument before super releases self.updater.
    // Missing observations are evidence, never an invented group or an exit
    // from inside Sparkle's callback. Let the real completion run first.
    LastProbeGroup = SnapshotNumber(GroupSnapshot(updater, @"probe", @"completion"));
    LastProbeSelection = self.selected.versionString ?: @"none";
    NSNumber *sourceGroup = SnapshotNumber(GroupSnapshot(TSUpdater, @"source", @"completion"));
    Trace([NSString stringWithFormat:@"rotation probe selected=%@ source-group=%@ probe-group=%@ error=%@/%ld",
           LastProbeSelection, GroupText(sourceGroup), GroupText(LastProbeGroup),
           error.domain ?: @"none", (long)error.code]);
    [super updater:updater didFinishUpdateCycleForUpdateCheck:check error:error];
    if (TSObserverIncomplete) dispatch_async(dispatch_get_main_queue(), ^{
        Trace(@"rotation observer-incomplete exit=62 after callback completion");
        exit(62);
    });
}
@end
extern void fixture_record_report(int kind);
extern bool fixture_about_retry_enabled(void);
static BOOL TSFirstTerminationResolved;
static BOOL TSRetryClicked;
static void Report(int kind, const char *text) {
    fixture_record_report(kind);
    if (kind == 7 && (TSDriver.retryTermination || okilum_updater_ready()
        || okilum_updater_guard_requested() || TSTermination.verifiedQuit)) exit(50);
    Trace([NSString stringWithFormat:@"report %d %s", kind, text ?: ""]);
    if (kind == 7) dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 2 * NSEC_PER_SEC), dispatch_get_main_queue(), ^{
        Trace(@"ordinary quit after cancellation");
        [NSApp terminate:nil];
    });
}
extern bool fixture_finish_verified_install(void);
bool fixture_protect_current_editor(void) {
    Trace(@"protect current editor after verification");
    if ([[NSBundle.mainBundle objectForInfoDictionaryKey:@"TSRefuse"] boolValue]
        || (TSRetryClicked && [[NSBundle.mainBundle objectForInfoDictionaryKey:@"TSRetryGuardRefuse"] boolValue])) return false;
    NSString *editor = [NSBundle.mainBundle objectForInfoDictionaryKey:@"TSEditor"];
    NSData *current = [NSData dataWithContentsOfFile:editor];
    NSString *protected = [NSBundle.mainBundle objectForInfoDictionaryKey:@"TSProtectedEditor"];
    return [current writeToFile:protected atomically:YES];
}
bool fixture_persist_ui_state(void) { Trace(@"persist UI after editor protection"); return true; }

// Test-only interception of the public AppKit window-ordering entry point.
// A real standard-driver check is the positive control before arming the probe.
static BOOL WindowProbeArmed;
static NSUInteger OrderedWindows;
@interface NSWindow (TSWindowProbe)
- (void)ts_orderWindow:(NSWindowOrderingMode)place relativeTo:(NSInteger)other;
@end
@implementation NSWindow (TSWindowProbe)
- (void)ts_orderWindow:(NSWindowOrderingMode)place relativeTo:(NSInteger)other {
    if (place != NSWindowOut) {
        OrderedWindows++;
        if (WindowProbeArmed) { Trace(@"unexpected updater window"); exit(48); }
    }
    [self ts_orderWindow:place relativeTo:other];
}
@end
static void ArmWindowProbe(NSBundle *bundle) {
    method_exchangeImplementations(class_getInstanceMethod(NSWindow.class, @selector(orderWindow:relativeTo:)),
        class_getInstanceMethod(NSWindow.class, @selector(ts_orderWindow:relativeTo:)));
    SPUStandardUserDriver *control = [[SPUStandardUserDriver alloc] initWithHostBundle:bundle delegate:nil];
    [control showUserInitiatedUpdateCheckWithCancellation:^{}];
    if (!OrderedWindows) exit(49);
    [control dismissUpdateInstallation];
    Trace(@"window probe positive control detected standard Sparkle check");
    WindowProbeArmed = YES;
}

static BOOL TSCompare;
static BOOL TSComparisonForeground;
static SUAppcastItem *TSActualSelection;
static void FinishComparison(void) {
    [TSProbe check:TSUpdater channels:TSChannels() foreground:TSComparisonForeground completion:^(SUAppcastItem *selected, NSError *error) {
        NSString *actual = TSActualSelection.versionString ?: @"none";
        NSString *probed = selected.versionString ?: @"none";
        BOOL samePayload = (!selected && !TSActualSelection) || [selected.fileURL isEqual:TSActualSelection.fileURL];
        if (![actual isEqualToString:probed] || !samePayload) exit(41);
        NSString *expected = [NSBundle.mainBundle objectForInfoDictionaryKey:@"TSExpectedSelection"];
        if (![actual isEqualToString:expected]) exit(42);
        if ([[NSBundle.mainBundle objectForInfoDictionaryKey:@"TSExpectDelta"] boolValue] && !TSActualSelection.deltaUpdate) exit(46);
        if (error && !([error.domain isEqualToString:SUSparkleErrorDomain] && error.code == SUNoUpdateError)) exit(43);
        Trace([NSString stringWithFormat:@"eligibility matches %@ %@", TSComparisonForeground ? @"foreground" : @"background", actual]);
        NSString *receipt = [NSBundle.mainBundle objectForInfoDictionaryKey:@"TSReceipt"];
        [@"eligibility matched" writeToFile:receipt atomically:YES encoding:NSUTF8StringEncoding error:NULL];
        exit(0);
    }];
}
@interface TestUpdaterDelegate : TSUpdaterDelegate
@end
@implementation TestUpdaterDelegate
- (void)updater:(SPUUpdater *)updater didFindValidUpdate:(SUAppcastItem *)item {
    if (RotatesGroup()) {
        NSNumber *group = SnapshotNumber(GroupSnapshot(updater, @"source", @"admission"));
        Trace([NSString stringWithFormat:@"rotation admitted=%@ source-group=%@", item.versionString, GroupText(group)]);
        if (![item.versionString isEqualToString:@"4"]) exit(58);
    }
    if (TSCompare) TSActualSelection = item;
    [super updater:updater didFindValidUpdate:item];
}
- (void)updater:(SPUUpdater *)updater didDownloadUpdate:(SUAppcastItem *)item {
    if (!RotatesGroup()) return;
    if (![item.versionString isEqualToString:@"4"]) exit(59);
    // Real public callback, after Sparkle's random post-download rotation. Make
    // that permitted transition deterministic only in this isolated test app.
    NSBundle *bundle = updater.hostBundle;
    NSString *domain = [bundle objectForInfoDictionaryKey:@"SUDefaultsDomain"];
    NSUserDefaults *preferences = domain ? [[NSUserDefaults alloc] initWithSuiteName:domain]
                                         : NSUserDefaults.standardUserDefaults;
    [preferences setObject:@6 forKey:@"SUUpdateGroupIdentifier"];
    if (![preferences synchronize]) exit(60);
    CFStringRef probeDomain = (__bridge CFStringRef)(domain ?: bundle.bundleIdentifier);
    CFPreferencesSetAppValue(CFSTR("SUUpdateGroupIdentifier"), (__bridge CFNumberRef)@6, probeDomain);
    if (!CFPreferencesAppSynchronize(probeDomain)) exit(60);
    id probeGroup = CFBridgingRelease(CFPreferencesCopyAppValue(CFSTR("SUUpdateGroupIdentifier"), probeDomain));
    NSNumber *sourceGroup = SnapshotNumber(GroupSnapshot(updater, @"source", @"download"));
    if (![probeGroup isKindOfClass:NSNumber.class] || ![probeGroup isEqual:@6]) exit(61);
    Trace([NSString stringWithFormat:@"rotation didDownloadUpdate=4 transition=0->6 source-readback=%@ probe-storage-readback=6", GroupText(sourceGroup)]);
}
- (void)updater:(SPUUpdater *)updater didFinishUpdateCycleForUpdateCheck:(SPUUpdateCheck)check error:(NSError *)error {
    Trace([NSString stringWithFormat:@"cycle finished check=%ld error=%@/%ld replacement=%d session=%d", (long)check, error.domain ?: @"none", (long)error.code, TSRestartAfterCycle, updater.sessionInProgress]);
    BOOL replacement = TSRestartAfterCycle && !error;
    [super updater:updater didFinishUpdateCycleForUpdateCheck:check error:error];
    if (replacement) {
        // The next session must be claimed inside completion, before Sparkle's
        // enabled automatic scheduler reserves it for interval discovery.
        if (!updater.sessionInProgress) exit(47);
        Trace(@"replacement session started before scheduling");
    }
    if (TSCompare) dispatch_async(dispatch_get_main_queue(), ^{ FinishComparison(); });
}
@end
@interface TestDriver : TSUserDriver
@end
@implementation TestDriver
- (void)showUpdaterError:(NSError *)error acknowledgement:(void (^)(void))acknowledgement {
    Trace([@"Sparkle error: " stringByAppendingString:error.localizedDescription]);
    [super showUpdaterError:error acknowledgement:acknowledgement];
}
- (void)showUpdateNotFoundWithError:(NSError *)error acknowledgement:(void (^)(void))acknowledgement {
    [super showUpdateNotFoundWithError:error acknowledgement:acknowledgement];
}
- (void)dismissUpdateInstallation {
    Trace([@"dismiss " stringByAppendingString:self.item.versionString ?: @"none"]);
    [super dismissUpdateInstallation];
    Trace(@"dismiss returned");
}
- (void)cancelForReplacement:(void (^)(SPUUserUpdateChoice))reply error:(NSError *)error {
    Trace([NSString stringWithFormat:@"cancel replacement error=%@/%ld", error.domain ?: @"none", (long)error.code]);
    if (RotatesGroup()) {
        Trace([NSString stringWithFormat:@"rotation rejection error=%@/%ld selected=%@ source-group=%@ probe-group=%@",
               error.domain ?: @"none", (long)error.code, LastProbeSelection,
               GroupText(SnapshotNumber(GroupSnapshot(TSUpdater, @"source", @"rejection"))), GroupText(LastProbeGroup)]);
    }
    [super cancelForReplacement:reply error:error];
    Trace(@"cancel reply returned");
}
- (void)showUserInitiatedUpdateCheckWithCancellation:(void (^)(void))cancellation {
    Trace(@"foreground check started");
    [super showUserInitiatedUpdateCheckWithCancellation:cancellation];
    if ([[NSBundle.mainBundle objectForInfoDictionaryKey:@"TSCancel"] isEqualToString:@"check"])
        dispatch_async(dispatch_get_main_queue(), ^{ okilum_updater_cancel(); });
}
- (void)presentUpdate:(SUAppcastItem *)item state:(SPUUserUpdateState *)state reply:(void (^)(SPUUserUpdateChoice))reply {
    Trace([@"offer " stringByAppendingString:item.versionString]);
    Trace(state.userInitiated ? @"selection foreground" : @"selection background");
    if (TSCompare) { reply(SPUUserUpdateChoiceDismiss); return; }
    [super presentUpdate:item state:state reply:reply];
    if ([[NSBundle.mainBundle objectForInfoDictionaryKey:@"TSDiscovery"] boolValue])
        dispatch_after(dispatch_time(DISPATCH_TIME_NOW, NSEC_PER_SEC), dispatch_get_main_queue(), ^{ [NSApp terminate:nil]; });
}
- (void)showDownloadInitiatedWithCancellation:(void (^)(void))cancellation {
    [super showDownloadInitiatedWithCancellation:cancellation];
    if ([[NSBundle.mainBundle objectForInfoDictionaryKey:@"TSCancel"] isEqualToString:@"download"])
        okilum_updater_cancel();
}
- (void)showDownloadDidStartExtractingUpdate {
    Trace([@"downloaded " stringByAppendingString:self.item.versionString]);
    [super showDownloadDidStartExtractingUpdate];
    if ([[NSBundle.mainBundle objectForInfoDictionaryKey:@"TSQuitDuringDownload"] boolValue]) {
        Trace(@"quit during download/extraction");
        [NSApp terminate:nil];
    }
}
- (void)showReadyToInstallAndRelaunch:(void (^)(SPUUserUpdateChoice))reply {
    Trace([@"prepared " stringByAppendingString:self.item.versionString]);
    if ([self.item.versionString isEqualToString:@"2"]) {
        // Publish only AFTER the real N download/extraction. The unchanged-feed
        // control copies the same feed; all scenarios use this exact hook.
        NSString *pending = [NSBundle.mainBundle objectForInfoDictionaryKey:@"TSPendingFeed"];
        NSData *data = [NSData dataWithContentsOfFile:pending];
        NSString *feed = [NSBundle.mainBundle objectForInfoDictionaryKey:@"TSFeedPath"];
        if (![data writeToFile:feed atomically:YES]) exit(31);
        Trace(@"published feed");
    }
    [super showReadyToInstallAndRelaunch:reply];
    if ([[NSBundle.mainBundle objectForInfoDictionaryKey:@"TSCancel"] isEqualToString:@"verifying"])
        okilum_updater_cancel();
}
- (void)presentReady:(void (^)(SPUUserUpdateChoice))reply {
    Trace([@"verified-ready " stringByAppendingString:self.item.versionString]);
    NSString *restartFeed = [NSBundle.mainBundle objectForInfoDictionaryKey:@"TSRestartFeed"];
    if (restartFeed && [self.item.versionString isEqualToString:@"2"]) {
        NSData *data = [NSData dataWithContentsOfFile:restartFeed];
        if (![data writeToFile:[NSBundle.mainBundle objectForInfoDictionaryKey:@"TSFeedPath"] atomically:YES]) exit(34);
        Trace(@"published while ready");
    }
    if (TSCompare) return;
    if ([[NSBundle.mainBundle objectForInfoDictionaryKey:@"TSCancel"] isEqualToString:@"ready"]) {
        okilum_updater_cancel();
        return;
    }
    if ([[NSBundle.mainBundle objectForInfoDictionaryKey:@"TSQuit"] boolValue]) {
        Trace(@"ordinary quit requested");
        [NSApp terminate:nil];
        Trace(@"ordinary quit returned to main queue");
        return;
    }
    reply(SPUUserUpdateChoiceInstall); // automated click on Restart
}
@end
@interface TestApplication : NSObject <NSApplicationDelegate>
@property(nonatomic) NSUInteger terminationAttempts;
@end
@implementation TestApplication
- (void)applicationWillTerminate:(NSNotification *)notification {
    if (NSApp.delegate != self) exit(35);
    Trace(@"original delegate restored for shutdown");
}
- (NSApplicationTerminateReply)applicationShouldTerminate:(NSApplication *)sender {
    Trace(@"termination requested");
    self.terminationAttempts++;
    NSString *mode = [NSBundle.mainBundle objectForInfoDictionaryKey:@"TSTerminationRetry"];
    if (mode.length && self.terminationAttempts == 1) {
        if ([mode isEqualToString:@"delay"]) {
            Trace(@"original delegate delayed first termination");
            NSTimer *decision = [NSTimer timerWithTimeInterval:0.2 repeats:NO block:^(NSTimer *timer) {
                Trace(@"original delegate resolved delayed termination NO");
                TSFirstTerminationResolved = YES;
                [sender replyToApplicationShouldTerminate:NO];
            }];
            // AppKit owns the deferred decision. Its modal run loop must be
            // able to deliver it without needing the blocked main queue.
            [NSRunLoop.mainRunLoop addTimer:decision forMode:NSDefaultRunLoopMode];
            [NSRunLoop.mainRunLoop addTimer:decision forMode:NSModalPanelRunLoopMode];
            return NSTerminateLater;
        }
        Trace(@"original delegate refused first termination");
        TSFirstTerminationResolved = YES;
        return NSTerminateCancel;
    }
    if (mode.length) {
        if (!TSRetryClicked || !TSFirstTerminationResolved) exit(51);
        Trace(@"original delegate accepted deliberate retry");
    }
    return NSTerminateNow;
}
@end
int main(void) {
    @autoreleasepool {
        [NSApplication sharedApplication];
        [NSApp setActivationPolicy:NSApplicationActivationPolicyAccessory];
        NSBundle *bundle = NSBundle.mainBundle;
        NSString *version = [bundle objectForInfoDictionaryKey:@"CFBundleVersion"];
        if (![version isEqualToString:@"1"]) {
            Trace([@"relaunched " stringByAppendingString:version]);
            NSString *receipt = [bundle objectForInfoDictionaryKey:@"TSReceipt"];
            [version writeToFile:receipt atomically:YES encoding:NSUTF8StringEncoding error:NULL];
            return 0;
        }
        TestApplication *delegate = [TestApplication new];
        NSApp.delegate = delegate;
        TSRegisterChannelDefault();
        // Also exercise migration: an old persisted automatic-download choice
        // must not bypass the production hook once the new plist disallows it.
        [NSUserDefaults.standardUserDefaults setBool:YES forKey:@"SUAutomaticallyUpdate"];
        [NSUserDefaults.standardUserDefaults setBool:[[bundle objectForInfoDictionaryKey:@"TSBeta"] boolValue] forKey:TSBetaKey];
        TSReporter = Report;
        TSCompare = [[bundle objectForInfoDictionaryKey:@"TSCompareEligibility"] boolValue];
        TSComparisonForeground = [[bundle objectForInfoDictionaryKey:@"TSForeground"] boolValue];
        {
            // Fixture inputs for pinned Sparkle, never product-side selection.
            // Pinned Sparkle 2.10.0 SUHost uses standard defaults for its main
            // bundle unless SUDefaultsDomain is explicitly supplied. Its own
            // bundle identifier is NOT an equivalent suite name on macOS.
            NSString *domain = [bundle objectForInfoDictionaryKey:@"SUDefaultsDomain"];
            NSUserDefaults *preferences = domain ? [[NSUserDefaults alloc] initWithSuiteName:domain]
                                                 : NSUserDefaults.standardUserDefaults;
            NSNumber *requestedGroup = [bundle objectForInfoDictionaryKey:@"TSPhaseGroup"];
            if (![requestedGroup isKindOfClass:NSNumber.class]
                || (requestedGroup.integerValue != 0 && requestedGroup.integerValue != 6)) return 33;
            [preferences setObject:requestedGroup forKey:@"SUUpdateGroupIdentifier"];
            [preferences synchronize];
            // Read precisely the store SUHost will use: the same standard
            // singleton, or a second instance for an explicit suite. This checks
            // in-process visibility, not independent on-disk persistence.
            NSUserDefaults *reader = domain ? [[NSUserDefaults alloc] initWithSuiteName:domain]
                                            : NSUserDefaults.standardUserDefaults;
            id effectiveGroup = [reader objectForKey:@"SUUpdateGroupIdentifier"];
            if (![effectiveGroup isKindOfClass:NSNumber.class] || ![effectiveGroup isEqual:requestedGroup]) return 34;
            Trace([NSString stringWithFormat:@"rollout group read=%@ domain=%@ before-updater-start",
                   effectiveGroup, domain ?: @"standard-main-bundle"]);
            // Test-only snapshot: the isolated production probe uses this suite,
            // while the main host without SUDefaultsDomain uses standard defaults.
            // Seed both explicitly; this does not repair production domain sharing.
            NSString *probeDomain = domain ?: bundle.bundleIdentifier;
            CFStringRef probeAppID = (__bridge CFStringRef)probeDomain;
            CFPreferencesSetAppValue(CFSTR("SUUpdateGroupIdentifier"),
                                     (__bridge CFNumberRef)requestedGroup, probeAppID);
            if (!CFPreferencesAppSynchronize(probeAppID)) return 34;
            id probeGroup = CFBridgingRelease(CFPreferencesCopyAppValue(
                CFSTR("SUUpdateGroupIdentifier"), probeAppID));
            if (![probeGroup isKindOfClass:NSNumber.class] || ![probeGroup isEqual:effectiveGroup]) return 34;
            Trace([NSString stringWithFormat:@"rollout probe group read=%@ domain=%@ matches-source before-updater-start",
                   probeGroup, probeDomain]);
            NSString *skip = [bundle objectForInfoDictionaryKey:@"TSSkip"];
            if (skip) [preferences setObject:skip forKey:@"SUSkippedVersion"];
            NSString *major = [bundle objectForInfoDictionaryKey:@"TSSkipMajor"];
            if (major) {
                [preferences setObject:major forKey:@"SUSkippedMajorVersion"];
                [preferences setObject:@"4" forKey:@"SUSkippedMajorSubreleaseVersion"];
            }
        }
        ArmWindowProbe(bundle);
        TSStart(bundle, [TestDriver new], [TestUpdaterDelegate new]);
        if (RotatesGroup()) TSProbe = [TestFreshnessProbe new];
        if (TSUpdater.automaticallyDownloadsUpdates || !TSUpdater.automaticallyChecksForUpdates) return 32;
        TSInstallRequested = !TSCompare && ![[bundle objectForInfoDictionaryKey:@"TSDiscovery"] boolValue]; // explicit About Install intent
        if (TSCompare && !TSComparisonForeground) [TSUpdater checkForUpdateInformation];
        else if ([[bundle objectForInfoDictionaryKey:@"TSBackgroundInstall"] boolValue]) [TSUpdater checkForUpdatesInBackground];
        else [TSUpdater checkForUpdates];
        [NSTimer scheduledTimerWithTimeInterval:0.05 repeats:YES block:^(NSTimer *timer) {
            if (TSFirstTerminationResolved && !TSRetryClicked && TSDriver.retryTermination) {
                if (!fixture_about_retry_enabled() || !okilum_updater_ready()) exit(52);
                Trace(@"About retry enabled after original delegate decision");
                // No synthetic Sparkle callback: click the production About action.
                TSRetryClicked = YES;
                if (!okilum_updater_restart()) exit(53);
                if (okilum_updater_restart()) exit(54); // no second attempt during verification
                Trace(@"deliberate About retry requested");
            }
            if (okilum_updater_guard_requested()) {
                Trace(@"final editor guard boundary");
                // A user edit made AFTER the successful fetch must be the
                // revision protected by the production Rust coordinator.
                NSString *editor = [bundle objectForInfoDictionaryKey:@"TSEditor"];
                NSString *revision = TSRetryClicked ? @"edited after retry verification" : @"edited after verification";
                if (![revision writeToFile:editor atomically:YES encoding:NSUTF8StringEncoding error:NULL]) exit(45);
                Trace(@"editor changed after verification");
                bool completed = fixture_finish_verified_install();
                BOOL refusal = [[bundle objectForInfoDictionaryKey:@"TSRefuse"] boolValue]
                    || (TSRetryClicked && [[bundle objectForInfoDictionaryKey:@"TSRetryGuardRefuse"] boolValue]);
                if (completed == refusal) exit(33);
                if (TSRetryClicked) {
                    Trace(completed ? @"retry editor guard committed" : @"retry editor guard refused");
                    if (!completed && (!fixture_about_retry_enabled() || !okilum_updater_ready())) exit(55);
                }
            }
        }];
        [NSApp run];
    }
    return 0;
}

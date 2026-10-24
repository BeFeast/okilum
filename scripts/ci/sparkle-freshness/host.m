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
static void Report(int kind, const char *text) {
    Trace([NSString stringWithFormat:@"report %d %s", kind, text ?: ""]);
    if (kind == 7) dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 2 * NSEC_PER_SEC), dispatch_get_main_queue(), ^{
        Trace(@"ordinary quit after cancellation");
        [NSApp terminate:nil];
    });
}
extern bool fixture_finish_verified_install(void);
bool fixture_protect_current_editor(void) {
    Trace(@"protect current editor after verification");
    if ([[NSBundle.mainBundle objectForInfoDictionaryKey:@"TSRefuse"] boolValue]) return false;
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
    if (TSCompare) TSActualSelection = item;
    [super updater:updater didFindValidUpdate:item];
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
@end
@implementation TestApplication
- (void)applicationWillTerminate:(NSNotification *)notification {
    if (NSApp.delegate != self) exit(35);
    Trace(@"original delegate restored for shutdown");
}
- (NSApplicationTerminateReply)applicationShouldTerminate:(NSApplication *)sender {
    Trace(@"termination requested");
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
            NSString *domain = [bundle objectForInfoDictionaryKey:@"SUDefaultsDomain"] ?: bundle.bundleIdentifier;
            NSUserDefaults *preferences = [[NSUserDefaults alloc] initWithSuiteName:domain];
            [preferences setInteger:6 forKey:@"SUUpdateGroupIdentifier"];
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
        if (TSUpdater.automaticallyDownloadsUpdates || !TSUpdater.automaticallyChecksForUpdates) return 32;
        TSInstallRequested = !TSCompare && ![[bundle objectForInfoDictionaryKey:@"TSDiscovery"] boolValue]; // explicit About Install intent
        if (TSCompare && !TSComparisonForeground) [TSUpdater checkForUpdateInformation];
        else if ([[bundle objectForInfoDictionaryKey:@"TSBackgroundInstall"] boolValue]) [TSUpdater checkForUpdatesInBackground];
        else [TSUpdater checkForUpdates];
        [NSTimer scheduledTimerWithTimeInterval:0.05 repeats:YES block:^(NSTimer *timer) {
            if (okilum_updater_guard_requested()) {
                Trace(@"final editor guard boundary");
                // A user edit made AFTER the successful fetch must be the
                // revision protected by the production Rust coordinator.
                NSString *editor = [bundle objectForInfoDictionaryKey:@"TSEditor"];
                if (![@"edited after verification" writeToFile:editor atomically:YES encoding:NSUTF8StringEncoding error:NULL]) exit(45);
                Trace(@"editor changed after verification");
                bool completed = fixture_finish_verified_install();
                if (completed == [[bundle objectForInfoDictionaryKey:@"TSRefuse"] boolValue]) exit(33);
            }
        }];
        [NSApp run];
    }
    return 0;
}

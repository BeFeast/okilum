// Stock Sparkle 2: standard updater controller, public API only. A manual
// check only probes the feed and reports the outcome to Rust, shown inline
// (#995); Sparkle's own UI appears only to install a found update.
#import <AppKit/AppKit.h>
#import <Sparkle/Sparkle.h>
#include <stdbool.h>

static NSString *const TSBetaKey = @"OkilumReceiveBetaBuilds";

// Every build follows stable unless the user chose Beta (#1307). Promotion
// re-signs a beta build without rebuilding it, so a beta default here moved
// every stable install to every-merge beta. A choice is the value the
// Settings toggle (or `defaults write`) stored; nothing else ever writes it.
static void TSRegisterChannelDefault(void) {
    [NSUserDefaults.standardUserDefaults registerDefaults:@{TSBetaKey : @NO}];
}

// kind: 0 up to date, 1 update available (text = version), 2 error (text = reason).
typedef void (*TSReport)(int kind, const char *text);
static TSReport TSReporter;
static void TSSend(int kind, NSString *text) {
    if (TSReporter) TSReporter(kind, (text ?: @"").UTF8String);
}

// A probe must have its own host identity: probing the installation updater
// resumes its retained installer instead of fetching the feed. Copy the host's
// version/security configuration, but never allow this metadata-only updater to
// schedule, download or install anything. Sparkle still selects/filter items.
@interface TSFreshnessProbe : NSObject <SPUUpdaterDelegate>
@property(nonatomic, strong) SPUUpdater *updater;
@property(nonatomic, strong) NSURL *directory;
@property(nonatomic, strong) SUAppcastItem *selected;
@property(nonatomic, copy) void (^completion)(SUAppcastItem *, NSError *);
@property(nonatomic, copy) NSSet<NSString *> *channels;
@property(nonatomic, copy) NSString *feed;
- (void)check:(SPUUpdater *)source channels:(NSSet<NSString *> *)channels
  userDriver:(id<SPUUserDriver>)userDriver completion:(void (^)(SUAppcastItem *, NSError *))completion;
@end
@implementation TSFreshnessProbe
- (void)check:(SPUUpdater *)source channels:(NSSet<NSString *> *)channels
  userDriver:(id<SPUUserDriver>)userDriver completion:(void (^)(SUAppcastItem *, NSError *))completion {
    NSAssert(NSThread.isMainThread, @"Sparkle is main-thread only");
    if (self.completion) {
        completion(nil, [NSError errorWithDomain:@"OkilumUpdate" code:6 userInfo:@{NSLocalizedDescriptionKey:@"An update verification is already in progress. Try again."}]);
        return;
    }
    self.channels = channels;
    self.completion = completion;
    self.selected = nil;
    NSError *error = nil;
    if (!self.updater) {
        self.directory = [[NSURL fileURLWithPath:NSTemporaryDirectory() isDirectory:YES]
            URLByAppendingPathComponent:NSUUID.UUID.UUIDString isDirectory:YES];
        NSURL *contents = [self.directory URLByAppendingPathComponent:@"Freshness.bundle/Contents" isDirectory:YES];
        NSMutableDictionary *info = [source.hostBundle.infoDictionary mutableCopy];
        info[@"CFBundleIdentifier"] = [source.hostBundle.bundleIdentifier stringByAppendingString:
            [@".freshness." stringByAppendingString:NSUUID.UUID.UUIDString]];
        info[@"SUEnableAutomaticChecks"] = @NO;
        info[@"SUAllowsAutomaticUpdates"] = @NO;
        [info removeObjectForKey:@"CFBundleExecutable"];
        if (![[NSFileManager defaultManager] createDirectoryAtURL:contents withIntermediateDirectories:YES attributes:nil error:&error]
            || ![info writeToURL:[contents URLByAppendingPathComponent:@"Info.plist"] error:&error]) {
            [self finish:error]; return;
        }
        NSBundle *bundle = [NSBundle bundleWithURL:[contents URLByDeletingLastPathComponent]];
        self.updater = [[SPUUpdater alloc] initWithHostBundle:bundle
            applicationBundle:source.hostBundle userDriver:userDriver delegate:self];
    }
    self.updater.httpHeaders = source.httpHeaders;
    self.updater.userAgentString = source.userAgentString;
    // The delegate supplies the source's effective URL, including any supported
    // existing preference. No override of signing/validation/selection is used.
    self.feed = source.feedURL.absoluteString;
    if (![self.updater startUpdater:&error]) { [self finish:error]; return; }
    [self.updater checkForUpdateInformation];
}
- (NSSet<NSString *> *)allowedChannelsForUpdater:(SPUUpdater *)updater { return self.channels; }
- (NSString *)feedURLStringForUpdater:(SPUUpdater *)updater { return self.feed; }
- (BOOL)updaterShouldPromptForPermissionToCheckForUpdates:(SPUUpdater *)updater { return NO; }
- (BOOL)updater:(SPUUpdater *)updater mayPerformUpdateCheck:(SPUUpdateCheck)check error:(NSError **)error {
    if (check == SPUUpdateCheckUpdateInformation) return YES;
    *error = [NSError errorWithDomain:@"OkilumUpdate" code:1 userInfo:@{NSLocalizedDescriptionKey:@"The freshness probe only reads update information."}];
    return NO;
}
- (void)updater:(SPUUpdater *)updater didFindValidUpdate:(SUAppcastItem *)item { self.selected = item; }
- (void)updater:(SPUUpdater *)updater didFinishUpdateCycleForUpdateCheck:(SPUUpdateCheck)check error:(NSError *)error {
    [self finish:error];
}
- (void)finish:(NSError *)error {
    void (^completion)(SUAppcastItem *, NSError *) = self.completion;
    self.completion = nil;
    if (completion) completion(self.selected, error);
}
- (void)dealloc {
    // The probe lives for the app's lifetime; it never owns canonical data.
    if (_directory) [[NSFileManager defaultManager] removeItemAtURL:_directory error:NULL];
}
@end

static SPUUpdater *TSUpdater;
@class TSUserDriver;
static TSUserDriver *TSDriver;
static TSFreshnessProbe *TSProbe;
static BOOL TSRestartAfterCycle;
static NSString *TSCycleFailure;
static BOOL TSGuardRequested;
static BOOL TSManualCheck;
static NSSet<NSString *> *TSChannels(void) {
    return [NSUserDefaults.standardUserDefaults boolForKey:TSBetaKey]
        ? [NSSet setWithObjects:@"stable", @"beta", nil] : [NSSet setWithObject:@"stable"];
}
static void TSCheckFreshness(SUAppcastItem *item, void (^completion)(BOOL, NSError *)) {
    NSSet<NSString *> *channels = TSChannels();
    NSString *feed = TSUpdater.feedURL.absoluteString;
    [TSProbe check:TSUpdater channels:channels userDriver:(id<SPUUserDriver>)TSDriver completion:^(SUAppcastItem *selected, NSError *error) {
        if (![channels isEqualToSet:TSChannels()] || ![feed isEqualToString:TSUpdater.feedURL.absoluteString])
            error = [NSError errorWithDomain:@"OkilumUpdate" code:5 userInfo:@{NSLocalizedDescriptionKey:@"The update channel changed during verification. Check again."}];
        if (!error && !selected) error = [NSError errorWithDomain:@"OkilumUpdate" code:2
            userInfo:@{NSLocalizedDescriptionKey:@"The update is no longer available on this channel. Check for updates again."}];
        NSComparisonResult comparison = [SUStandardVersionComparator.defaultComparator compareVersion:selected.versionString ?: @"" toVersion:item.versionString];
        if (!error && comparison == NSOrderedAscending)
            error = [NSError errorWithDomain:@"OkilumUpdate" code:3
                userInfo:@{NSLocalizedDescriptionKey:@"The current feed is older than the downloaded update. Nothing was installed."}];
        completion(!error && comparison == NSOrderedSame, error);
    }];
}

@interface TSUpdaterDelegate : NSObject <SPUUpdaterDelegate>
@end
@implementation TSUpdaterDelegate
// Stable items carry the "stable" channel, so builds that predate this bridge
// (which only allowed "stable") keep seeing them.
- (NSSet<NSString *> *)allowedChannelsForUpdater:(SPUUpdater *)updater {
    return TSChannels();
}
- (void)updater:(SPUUpdater *)updater didFindValidUpdate:(SUAppcastItem *)item {
    TSSend(1, item.displayVersionString);
}
- (void)updater:(SPUUpdater *)updater didFinishUpdateCycleForUpdateCheck:(SPUUpdateCheck)check error:(NSError *)error {
    // Sparkle has released its old driver and completed cancellation before we
    // start another cycle. Never check reentrantly from a user-driver reply.
    BOOL restart = TSRestartAfterCycle && !error;
    TSRestartAfterCycle = NO;
    if (TSCycleFailure) { TSSend(2, TSCycleFailure); TSCycleFailure = nil; }
    if (restart) dispatch_async(dispatch_get_main_queue(), ^{ [TSUpdater checkForUpdates]; });
}
- (void)updaterDidNotFindUpdate:(SPUUpdater *)updater {
    TSSend(0, nil);
}
- (void)updater:(SPUUpdater *)updater didAbortWithError:(NSError *)error {
    // "No update" can also arrive as an error; it is not a failure.
    if ([error.domain isEqualToString:SUSparkleErrorDomain] && error.code == SUNoUpdateError) {
        TSSend(0, nil);
    } else {
        TSSend(2, error.localizedDescription);
    }
}
@end

// Keep Sparkle's standard UI and own only its public freshness/cancellation
// continuations. Sparkle still validates and installs every archive.
@interface TSUserDriver : SPUStandardUserDriver
@property(nonatomic) uint64_t expected;
@property(nonatomic) uint64_t received;
@property(nonatomic, copy) void (^restartReply)(SPUUserUpdateChoice);
@property(nonatomic, strong) SUAppcastItem *item;
@property(nonatomic) BOOL verifying;
@property(nonatomic) BOOL verifiedRestart;
@property(nonatomic) BOOL replacing;
@property(nonatomic) BOOL preparing;
@property(nonatomic) NSUInteger generation;
@property(nonatomic, copy) void (^quitReply)(BOOL);
- (void)verifyQuit:(void (^)(BOOL))reply;
- (void)requestRestart;
- (void)presentReady:(void (^)(SPUUserUpdateChoice))reply;
- (void)presentUpdate:(SUAppcastItem *)item state:(SPUUserUpdateState *)state reply:(void (^)(SPUUserUpdateChoice))reply;
- (void)cancelForReplacement:(void (^)(SPUUserUpdateChoice))reply error:(NSError *)error;
@end
@implementation TSUserDriver
- (void)showUpdateFoundWithAppcastItem:(SUAppcastItem *)item state:(SPUUserUpdateState *)state reply:(void (^)(SPUUserUpdateChoice))reply {
    self.item = item;
    // A pre-existing installer (from a previous build) also enters the hook.
    if (state.stage == SPUUserUpdateStageInstalling) {
        [self showReadyToInstallAndRelaunch:reply]; return;
    }
    [self presentUpdate:item state:state reply:reply];
}
- (void)presentUpdate:(SUAppcastItem *)item state:(SPUUserUpdateState *)state reply:(void (^)(SPUUserUpdateChoice))reply {
    // An explicit Install intent survives supersession, not a user cancellation.
    if (self.replacing && !item.informationOnlyUpdate && !item.majorUpgrade
        && item.signingValidationStatus != SPUAppcastSigningValidationStatusFailed) {
        self.replacing = NO;
        reply(SPUUserUpdateChoiceInstall);
    } else {
        self.replacing = NO;
        [super showUpdateFoundWithAppcastItem:item state:state reply:reply];
    }
}
- (void)cancelForReplacement:(void (^)(SPUUserUpdateChoice))reply error:(NSError *)error {
    self.restartReply = nil;
    self.verifiedRestart = NO;
    TSGuardRequested = NO;
    if (self.quitReply) { void (^quit)(BOOL) = self.quitReply; self.quitReply = nil; quit(NO); }
    TSRestartAfterCycle = error == nil;
    self.replacing = error == nil;
    TSCycleFailure = error.localizedDescription;
    // At Ready this cancels installation without recording a user skip.
    // Sparkle removes the old archive/installer; we never delete its files.
    reply(SPUUserUpdateChoiceSkip);
}
- (void)showDownloadInitiatedWithCancellation:(void (^)(void))cancellation {
    self.preparing = YES;
    self.expected = 0; self.received = 0;
    TSSend(3, @"");
    [super showDownloadInitiatedWithCancellation:cancellation];
}
- (void)showDownloadDidReceiveExpectedContentLength:(uint64_t)length {
    self.expected = length;
    [super showDownloadDidReceiveExpectedContentLength:length];
}
- (void)showDownloadDidReceiveDataOfLength:(uint64_t)length {
    self.received += length;
    if (self.expected) TSSend(3, [NSString stringWithFormat:@"%u", (unsigned)MIN(100.0, 100.0 * self.received / self.expected)]);
    [super showDownloadDidReceiveDataOfLength:length];
}
- (void)showDownloadDidStartExtractingUpdate {
    self.preparing = YES;
    TSSend(4, nil);
    [super showDownloadDidStartExtractingUpdate];
}
- (void)showReadyToInstallAndRelaunch:(void (^)(SPUUserUpdateChoice))reply {
    self.restartReply = reply;
    self.verifying = YES;
    NSUInteger generation = self.generation;
    TSSend(4, nil);
    __weak TSUserDriver *weakSelf = self;
    TSCheckFreshness(self.item, ^(BOOL current, NSError *error) {
        TSUserDriver *driver = weakSelf;
        if (!driver || driver.generation != generation) return;
        driver.verifying = NO;
        if (!current) { [driver cancelForReplacement:reply error:error]; return; }
        driver.restartReply = reply;
        if (driver.quitReply) {
            TSSend(5, nil);
            void (^quit)(BOOL) = driver.quitReply; driver.quitReply = nil; quit(YES); return;
        }
        TSSend(5, nil);
        [driver presentReady:^(SPUUserUpdateChoice choice) {
            if (!driver.restartReply || driver.verifying) return;
            if (choice == SPUUserUpdateChoiceInstall) { [driver requestRestart]; return; }
            void (^continuation)(SPUUserUpdateChoice) = driver.restartReply;
            driver.restartReply = nil;
            TSSend(7, nil);
            // Dismiss must not leave an implicit install-on-quit armed.
            continuation(SPUUserUpdateChoiceSkip);
        }];
    });
}
- (void)presentReady:(void (^)(SPUUserUpdateChoice))reply {
    [super showReadyToInstallAndRelaunch:reply];
}
- (void)requestRestart {
    if (!self.restartReply || self.verifying) return;
    self.verifying = YES;
    self.verifiedRestart = NO;
    TSGuardRequested = NO;
    NSUInteger generation = self.generation;
    TSSend(4, nil);
    __weak TSUserDriver *weakSelf = self;
    TSCheckFreshness(self.item, ^(BOOL current, NSError *error) {
        TSUserDriver *driver = weakSelf;
        if (!driver || driver.generation != generation) return;
        driver.verifying = NO;
        if (!driver.restartReply) return;
        if (!current) { [driver cancelForReplacement:driver.restartReply error:error]; return; }
        if (driver.quitReply) {
            TSSend(5, nil);
            void (^quit)(BOOL) = driver.quitReply; driver.quitReply = nil; quit(YES); return;
        }
        driver.verifiedRestart = YES;
        TSGuardRequested = YES;
        // Rust reruns editor protection/UI persistence after the async check,
        // for both Sparkle's dialog and Okilum's Restart action.
        TSSend(5, nil);
    });
}
- (void)verifyQuit:(void (^)(BOOL))reply {
    self.quitReply = reply;
    // A check already running after download/restart also covers this quit.
    // Its completion must reply to AppKit, never install ahead of verification.
    if (self.restartReply && !self.verifying) [self requestRestart];
}
- (void)showUpdateInstalledAndRelaunched:(BOOL)relaunched acknowledgement:(void (^)(void))acknowledgement {
    if (relaunched) TSSend(8, nil);
    [super showUpdateInstalledAndRelaunched:relaunched acknowledgement:acknowledgement];
}
- (void)dismissUpdateInstallation {
    self.generation += 1;
    self.preparing = NO;
    self.verifying = NO;
    if (self.quitReply) { void (^quit)(BOOL) = self.quitReply; self.quitReply = nil; quit(NO); }
    self.restartReply = nil;
    self.verifiedRestart = NO;
    TSGuardRequested = NO;
    [super dismissUpdateInstallation];
}
@end
// Sparkle also installs prepared interactive updates on ordinary Quit. Use
// AppKit's public asynchronous termination contract so this path cannot exit
// during verification or install an old prepared update after a long UI wait.
// Forward every other delegate callback unchanged; no GPUI/source patch.
@interface TSTerminationDelegate : NSObject <NSApplicationDelegate>
@property(nonatomic, strong) id<NSApplicationDelegate> original;
@end
@implementation TSTerminationDelegate
- (BOOL)respondsToSelector:(SEL)selector {
    return [super respondsToSelector:selector] || [self.original respondsToSelector:selector];
}
- (id)forwardingTargetForSelector:(SEL)selector { return self.original; }
- (void)applicationWillTerminate:(NSNotification *)notification {
    // GPUI reads its original delegate's native ivar after NSApp.run returns.
    // Restore that exact object before forwarding shutdown; never impersonate
    // or copy GPUI's private native state into this adapter.
    NSApp.delegate = self.original;
    if ([self.original respondsToSelector:_cmd]) [self.original applicationWillTerminate:notification];
}
- (NSApplicationTerminateReply)applicationShouldTerminate:(NSApplication *)app {
    if (!TSDriver.restartReply && !TSDriver.preparing) {
        return [self.original respondsToSelector:_cmd] ? [self.original applicationShouldTerminate:app] : NSTerminateNow;
    }
    [TSDriver verifyQuit:^(BOOL current) {
        if (!current) { [app replyToApplicationShouldTerminate:NO]; return; }
        NSApplicationTerminateReply decision = [self.original respondsToSelector:@selector(applicationShouldTerminate:)]
            ? [self.original applicationShouldTerminate:app] : NSTerminateNow;
        // An existing async delegate retains its own responsibility to reply.
        if (decision != NSTerminateLater) [app replyToApplicationShouldTerminate:decision == NSTerminateNow];
    }];
    return NSTerminateLater;
}
@end
static TSTerminationDelegate *TSTermination;
static TSUpdaterDelegate *TSDelegate;

static void TSStart(NSBundle *bundle, TSUserDriver *driver) {
    TSTermination = [TSTerminationDelegate new];
    TSTermination.original = NSApp.delegate;
    NSApp.delegate = TSTermination;
    TSProbe = [TSFreshnessProbe new];
    TSDelegate = [TSUpdaterDelegate new];
    TSDriver = driver;
    TSUpdater = [[SPUUpdater alloc] initWithHostBundle:bundle
                                 applicationBundle:bundle
                                        userDriver:TSDriver delegate:TSDelegate];
    NSError *error = nil;
    if (![TSUpdater startUpdater:&error]) TSSend(2, error.localizedDescription);
}
void okilum_updater_start(void) {
    TSRegisterChannelDefault();
    // A bare `cargo run` binary has no bundle feed; Sparkle would only report errors.
    if (TSUpdater || ![NSBundle.mainBundle objectForInfoDictionaryKey:@"SUFeedURL"]) return;
    TSStart(NSBundle.mainBundle, [[TSUserDriver alloc] initWithHostBundle:NSBundle.mainBundle delegate:nil]);
}

bool okilum_updater_available(void) { return TSUpdater != nil; }

void okilum_updater_set_report(TSReport report) { TSReporter = report; }

// False while a check or download session is already running: Sparkle would
// ignore the probe and never call the delegate.
bool okilum_updater_check(void) {
    if (TSUpdater.sessionInProgress || TSProbe.completion || TSManualCheck) return false;
    TSManualCheck = YES;
    dispatch_async(dispatch_get_main_queue(), ^{
        [TSProbe check:TSUpdater channels:TSChannels() userDriver:TSDriver completion:^(SUAppcastItem *selected, NSError *error) {
            TSManualCheck = NO;
            if (error && !([error.domain isEqualToString:SUSparkleErrorDomain] && error.code == SUNoUpdateError))
                TSSend(2, error.localizedDescription);
            else if (selected) TSSend(1, selected.displayVersionString);
            else TSSend(0, nil);
        }];
    });
    return true;
}

// Sparkle's standard UI downloads, installs and relaunches.
void okilum_updater_install(void) { [TSUpdater checkForUpdates]; }

bool okilum_updater_beta(void) {
    TSRegisterChannelDefault();
    return [NSUserDefaults.standardUserDefaults boolForKey:TSBetaKey];
}

void okilum_updater_set_beta(bool enabled) {
    [NSUserDefaults.standardUserDefaults setBool:enabled forKey:TSBetaKey];
}

// Tests only: an install that never chose a channel.
void okilum_updater_forget_beta(void) {
    [NSUserDefaults.standardUserDefaults removeObjectForKey:TSBetaKey];
}

// Called on main only after the Rust save/recovery guards succeed.
bool okilum_updater_restart(void) {
    if (!TSDriver.restartReply || TSDriver.verifying) return false;
    if (!TSGuardRequested) [TSDriver requestRestart];
    return true;
}
// Separate entry point: only Rust's post-verification editor/save guards may
// consume the install continuation. Repeated UI clicks cannot bypass them.
bool okilum_updater_finish_restart(void) {
    if (!TSDriver.restartReply || TSDriver.verifying || !TSDriver.verifiedRestart || !TSGuardRequested) return false;
    TSDriver.verifiedRestart = NO;
    TSDriver.preparing = NO;
    TSGuardRequested = NO;
    void (^continuation)(SPUUserUpdateChoice) = TSDriver.restartReply;
    TSDriver.restartReply = nil;
    // Close the standard driver UI and clear its stored handlers before consuming
    // our retained continuation. This public cleanup does not cancel installation.
    [TSDriver dismissUpdateInstallation];
    TSSend(6, nil);
    continuation(SPUUserUpdateChoiceInstall);
    return true;
}
bool okilum_updater_ready(void) { return TSDriver.restartReply != nil; }

// Polled on the GPUI main thread; the guard runs only after freshness succeeds.
bool okilum_updater_guard_requested(void) { return TSGuardRequested; }
void okilum_updater_refuse_restart(void) {
    TSGuardRequested = NO;
    if (TSDriver.restartReply) {
        NSError *error = [NSError errorWithDomain:@"OkilumUpdate" code:4 userInfo:@{NSLocalizedDescriptionKey:@"Your edits could not be protected. Nothing was installed."}];
        [TSDriver cancelForReplacement:TSDriver.restartReply error:error];
    }
}

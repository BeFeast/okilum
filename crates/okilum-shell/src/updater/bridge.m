// Sparkle owns selection, validation and installation. Okilum owns all update
// presentation through Rust About; neither updater creates standard windows.
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
typedef bool (*TSAutomaticAdmission)(bool automatic);
static TSAutomaticAdmission TSAdmitAutomatic;
static void TSSend(int kind, NSString *text) {
    if (TSReporter) TSReporter(kind, (text ?: @"").UTF8String);
}

// Foreground metadata checks must use Sparkle's foreground selector, not its
// always-background information probe. This driver never accepts a download.
@interface TSMetadataDriver : NSObject <SPUUserDriver>
@end
@implementation TSMetadataDriver
- (void)showUpdatePermissionRequest:(SPUUpdatePermissionRequest *)request reply:(void (^)(SUUpdatePermissionResponse *))reply {
    // The app already configures automatic checks. Never opt into downloads.
    reply([[SUUpdatePermissionResponse alloc] initWithAutomaticUpdateChecks:YES automaticUpdateDownloading:@NO sendSystemProfile:NO]);
}
- (void)showUserInitiatedUpdateCheckWithCancellation:(void (^)(void))cancellation {}
- (void)showUpdateFoundWithAppcastItem:(SUAppcastItem *)item state:(SPUUserUpdateState *)state reply:(void (^)(SPUUserUpdateChoice))reply { reply(SPUUserUpdateChoiceDismiss); }
- (void)showUpdateReleaseNotesWithDownloadData:(SPUDownloadData *)data {}
- (void)showUpdateReleaseNotesFailedToDownloadWithError:(NSError *)error {}
- (void)showUpdateNotFoundWithError:(NSError *)error acknowledgement:(void (^)(void))acknowledgement { acknowledgement(); }
- (void)showUpdaterError:(NSError *)error acknowledgement:(void (^)(void))acknowledgement { acknowledgement(); }
- (void)showDownloadInitiatedWithCancellation:(void (^)(void))cancellation { cancellation(); }
- (void)showDownloadDidReceiveExpectedContentLength:(uint64_t)length {}
- (void)showDownloadDidReceiveDataOfLength:(uint64_t)length {}
- (void)showDownloadDidStartExtractingUpdate {}
- (void)showExtractionReceivedProgress:(double)progress {}
- (void)showReadyToInstallAndRelaunch:(void (^)(SPUUserUpdateChoice))reply { reply(SPUUserUpdateChoiceSkip); }
- (void)showInstallingUpdateWithApplicationTerminated:(BOOL)terminated retryTerminatingApplication:(void (^)(void))retry {}
- (void)showUpdateInstalledAndRelaunched:(BOOL)relaunched acknowledgement:(void (^)(void))acknowledgement { acknowledgement(); }
- (void)dismissUpdateInstallation {}
@end

// A downloaded update has already passed Sparkle's admission filters. Keep
// that admission only for the same signed item collection, URL and channels.
// Public appcast callbacks expose all item properties, not the raw XML bytes.
// No preference, date or private selector is changed by this receipt.
static NSArray<NSDictionary *> *TSSignedItems(SUAppcast *appcast) {
    if (appcast.signingValidationStatus != SPUAppcastSigningValidationStatusSucceeded) return nil;
    NSMutableArray<NSDictionary *> *items = [NSMutableArray array];
    for (SUAppcastItem *item in appcast.items) {
        if (item.signingValidationStatus != SPUAppcastSigningValidationStatusSucceeded) return nil;
        [items addObject:[item.propertiesDictionary copy]];
    }
    return [items copy];
}
@interface TSAdmission : NSObject
@property(nonatomic, copy) NSArray<NSDictionary *> *items;
@property(nonatomic, copy) NSDictionary *item;
@property(nonatomic, copy) NSString *version;
@property(nonatomic, copy) NSURL *archive;
@property(nonatomic, copy) NSString *feed;
@property(nonatomic, copy) NSSet<NSString *> *channels;
- (BOOL)covers:(SUAppcastItem *)item feed:(NSString *)feed channels:(NSSet<NSString *> *)channels
    signedItems:(NSArray<NSDictionary *> *)items;
@end
@implementation TSAdmission
- (BOOL)covers:(SUAppcastItem *)item feed:(NSString *)feed channels:(NSSet<NSString *> *)channels
    signedItems:(NSArray<NSDictionary *> *)items {
    return self.items != nil && items != nil && [self.items isEqualToArray:items]
        && item.signingValidationStatus == SPUAppcastSigningValidationStatusSucceeded
        && !item.informationOnlyUpdate && !item.majorUpgrade
        && [self.item isEqualToDictionary:item.propertiesDictionary]
        && [self.version isEqualToString:item.versionString] && [self.archive isEqual:item.fileURL]
        && [self.feed isEqualToString:feed] && [self.channels isEqualToSet:channels];
}
@end
static TSAdmission *TSPendingAdmission;
static TSAdmission *TSDownloadedAdmission;
static void TSClearAdmission(void) { TSPendingAdmission = nil; TSDownloadedAdmission = nil; }

// A probe must have its own host identity: probing the installation updater
// resumes its retained installer instead of fetching the feed. Copy the host's
// version/security configuration, but never allow this metadata-only updater to
// download or install anything. Each probe updater is released on completion
// so it cannot retain a second scheduler. Sparkle still selects/filters items.
@interface TSFreshnessProbe : NSObject <SPUUpdaterDelegate>
@property(nonatomic, strong) SPUUpdater *updater;
@property(nonatomic, strong) NSURL *directory;
@property(nonatomic, strong) SUAppcastItem *selected;
@property(nonatomic, copy) void (^completion)(SUAppcastItem *, NSError *);
@property(nonatomic, copy) NSSet<NSString *> *channels;
@property(nonatomic, copy) NSString *feed;
@property(nonatomic) BOOL foreground;
@property(nonatomic, copy) NSArray<NSDictionary *> *signedItems;
- (void)check:(SPUUpdater *)source channels:(NSSet<NSString *> *)channels
  foreground:(BOOL)foreground completion:(void (^)(SUAppcastItem *, NSError *))completion;
@end
@implementation TSFreshnessProbe
- (void)check:(SPUUpdater *)source channels:(NSSet<NSString *> *)channels
  foreground:(BOOL)foreground completion:(void (^)(SUAppcastItem *, NSError *))completion {
    NSAssert(NSThread.isMainThread, @"Sparkle is main-thread only");
    if (self.completion) {
        completion(nil, [NSError errorWithDomain:@"OkilumUpdate" code:6 userInfo:@{NSLocalizedDescriptionKey:@"An update verification is already in progress. Try again."}]);
        return;
    }
    self.foreground = foreground;
    self.channels = channels;
    self.completion = completion;
    self.selected = nil;
    self.signedItems = nil;
    NSError *error = nil;
    if (!self.directory) {
        self.directory = [[NSURL fileURLWithPath:NSTemporaryDirectory() isDirectory:YES]
            URLByAppendingPathComponent:NSUUID.UUID.UUIDString isDirectory:YES];
        NSURL *contents = [self.directory URLByAppendingPathComponent:@"Freshness.bundle/Contents" isDirectory:YES];
        NSMutableDictionary *info = [source.hostBundle.infoDictionary mutableCopy];
        info[@"CFBundleIdentifier"] = [source.hostBundle.bundleIdentifier stringByAppendingString:
            [@".freshness." stringByAppendingString:NSUUID.UUID.UUIDString]];
        // SUDefaultsDomain is Sparkle's supported preference-domain contract.
        // Keep the source's actual skip/rollout (including a not-yet-assigned
        // group) and validation state. Do not invent/copy private preference keys.
        info[@"SUDefaultsDomain"] = [source.hostBundle objectForInfoDictionaryKey:@"SUDefaultsDomain"] ?: source.hostBundle.bundleIdentifier;
        info[@"SUEnableAutomaticChecks"] = @NO;
        info[@"SUAllowsAutomaticUpdates"] = @NO;
        [info removeObjectForKey:@"CFBundleExecutable"];
        if (![[NSFileManager defaultManager] createDirectoryAtURL:contents withIntermediateDirectories:YES attributes:nil error:&error]
            || ![info writeToURL:[contents URLByAppendingPathComponent:@"Info.plist"] error:&error]) {
            [self finish:error]; return;
        }
        // Delta eligibility checks inspect this framework's permissions, size
        // and locales. Point at the real framework rather than an empty bundle.
        NSURL *frameworks = [contents URLByAppendingPathComponent:@"Frameworks" isDirectory:YES];
        if (![[NSFileManager defaultManager] createDirectoryAtURL:frameworks withIntermediateDirectories:YES attributes:nil error:&error]
            || ![[NSFileManager defaultManager] createSymbolicLinkAtURL:[frameworks URLByAppendingPathComponent:@"Sparkle.framework"]
                withDestinationURL:[NSBundle bundleForClass:SPUUpdater.class].bundleURL error:&error]) {
            [self finish:error]; return;
        }
    }
    NSBundle *bundle = [NSBundle bundleWithURL:[self.directory URLByAppendingPathComponent:@"Freshness.bundle"]];
    TSMetadataDriver *driver = [TSMetadataDriver new];
    self.updater = [[SPUUpdater alloc] initWithHostBundle:bundle
        applicationBundle:source.hostBundle userDriver:driver delegate:self];
    self.updater.httpHeaders = source.httpHeaders;
    self.updater.userAgentString = source.userAgentString;
    // The delegate supplies the source's effective URL, including any supported
    // existing preference. No override of signing/validation/selection is used.
    self.feed = source.feedURL.absoluteString;
    if (![self.updater startUpdater:&error]) { [self finish:error]; return; }
    if (foreground) [self.updater checkForUpdates];
    else [self.updater checkForUpdateInformation];
}
- (NSSet<NSString *> *)allowedChannelsForUpdater:(SPUUpdater *)updater { return self.channels; }
- (NSString *)feedURLStringForUpdater:(SPUUpdater *)updater { return self.feed; }
- (BOOL)updaterShouldPromptForPermissionToCheckForUpdates:(SPUUpdater *)updater { return NO; }
- (BOOL)updater:(SPUUpdater *)updater mayPerformUpdateCheck:(SPUUpdateCheck)check error:(NSError **)error {
    if (check == (self.foreground ? SPUUpdateCheckUpdates : SPUUpdateCheckUpdateInformation)) return YES;
    *error = [NSError errorWithDomain:@"OkilumUpdate" code:1 userInfo:@{NSLocalizedDescriptionKey:@"The freshness probe only reads update information."}];
    return NO;
}
- (void)updater:(SPUUpdater *)updater didFinishLoadingAppcast:(SUAppcast *)appcast {
    self.signedItems = TSSignedItems(appcast);
}
- (void)updater:(SPUUpdater *)updater didFindValidUpdate:(SUAppcastItem *)item { self.selected = item; }
- (void)updater:(SPUUpdater *)updater didFinishUpdateCycleForUpdateCheck:(SPUUpdateCheck)check error:(NSError *)error {
    [self finish:error];
}
- (void)finish:(NSError *)error {
    void (^completion)(SUAppcastItem *, NSError *) = self.completion;
    self.completion = nil;
    // Sharing selection preferences must not leave a second updater scheduling
    // background checks. SPUUpdater invalidates its weak-delegate timer on dealloc.
    self.updater = nil;
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
static BOOL TSReplacementForeground;
static NSString *TSCycleFailure;
static BOOL TSGuardRequested;
static BOOL TSManualCheck;
static BOOL TSInstallRequested;
static BOOL TSBackgroundDownloads;
static BOOL TSAutomaticChecks = YES;
static BOOL TSCancelledCycle;
static BOOL TSScheduledCheckSkipped;
// #2679: Restart now was chosen, but the update it was shown for has been
// superseded (or is still downloading). The consent carries over to the newest
// update: Okilum restarts into it as soon as it is verified. Survives Sparkle's
// per-session dismissUpdateInstallation; cleared by cancel, failure or no update.
static BOOL TSRestartWhenReady;
static NSString *const TSDownloadingLatest = @"Downloading the latest update. Okilum will restart into it automatically when it is ready.";
static void TSEndRestartIntent(NSString *reason) {
    if (!TSRestartWhenReady) return;
    TSRestartWhenReady = NO;
    // A real outcome, shown in the single update notice with one action.
    if (reason) TSSend(14, reason);
}
static NSString *const TSNothingNewer = @"No newer update is ready to install. Okilum is up to date.";
static void (^TSQuitAfterCycle)(BOOL);
static BOOL TSReplacingUpdate(void);
static void TSEndReplacement(void);
static BOOL TSRetainsInstallation(void);
static NSSet<NSString *> *TSChannels(void) {
    return [NSUserDefaults.standardUserDefaults boolForKey:TSBetaKey]
        ? [NSSet setWithObjects:@"stable", @"beta", nil] : [NSSet setWithObject:@"stable"];
}
static void TSCheckFreshness(SUAppcastItem *item, BOOL foreground, void (^completion)(BOOL, NSError *)) {
    NSSet<NSString *> *channels = TSChannels();
    NSString *feed = TSUpdater.feedURL.absoluteString;
    TSAdmission *admission = TSDownloadedAdmission;
    [TSProbe check:TSUpdater channels:channels foreground:foreground completion:^(SUAppcastItem *selected, NSError *error) {
        if (![channels isEqualToSet:TSChannels()] || ![feed isEqualToString:TSUpdater.feedURL.absoluteString])
            error = [NSError errorWithDomain:@"OkilumUpdate" code:5 userInfo:@{NSLocalizedDescriptionKey:@"The update channel changed during verification. Check again."}];
        if (admission && (![admission.feed isEqualToString:feed] || ![admission.channels isEqualToSet:channels]))
            error = [NSError errorWithDomain:@"OkilumUpdate" code:5 userInfo:@{NSLocalizedDescriptionKey:@"The update channel or URL changed after download. Check again."}];
        // A no-update result is not a fetch/parse/signature failure. Only a
        // receipt from a real download can preserve the previous admission;
        // fresh discovery still uses Sparkle's current eligibility unchanged.
        BOOL noUpdate = [error.domain isEqualToString:SUSparkleErrorDomain] && error.code == SUNoUpdateError;
        BOOL notSuperseded = !selected || [SUStandardVersionComparator.defaultComparator
            compareVersion:selected.versionString toVersion:item.versionString] != NSOrderedDescending;
        if ((!error || noUpdate) && notSuperseded && admission && admission == TSDownloadedAdmission
            && [admission covers:item feed:TSUpdater.feedURL.absoluteString channels:TSChannels()
                signedItems:TSProbe.signedItems]) {
            completion(YES, nil);
            return;
        }
        if (!error && !selected) error = [NSError errorWithDomain:@"OkilumUpdate" code:2
            userInfo:@{NSLocalizedDescriptionKey:@"The update is no longer available on this channel. Check for updates again."}];
        NSComparisonResult comparison = [SUStandardVersionComparator.defaultComparator compareVersion:selected.versionString ?: @"" toVersion:item.versionString];
        if (!error && comparison == NSOrderedAscending)
            error = [NSError errorWithDomain:@"OkilumUpdate" code:3
                userInfo:@{NSLocalizedDescriptionKey:@"The current feed is older than the downloaded update. Nothing was installed."}];
        if (!error && comparison == NSOrderedSame && (![selected.fileURL isEqual:item.fileURL]
            || ![selected.propertiesDictionary[@"enclosure"] isEqual:item.propertiesDictionary[@"enclosure"]]))
            error = [NSError errorWithDomain:@"OkilumUpdate" code:7
                userInfo:@{NSLocalizedDescriptionKey:@"The downloaded update's archive changed. Nothing was installed."}];
        completion(!error && comparison == NSOrderedSame, error);
    }];
}

@interface TSUpdaterDelegate : NSObject <SPUUpdaterDelegate>
@property(nonatomic, copy) NSArray<NSDictionary *> *signedItems;
@property(nonatomic, copy) NSString *feed;
@property(nonatomic, copy) NSSet<NSString *> *channels;
@end
@implementation TSUpdaterDelegate
- (BOOL)updater:(SPUUpdater *)updater mayPerformUpdateCheck:(SPUUpdateCheck)check error:(NSError **)error {
    // Explicit Install and freshness-driven replacements retain their intent.
    // A native scheduled check shares the focus/timer throttle; probing must
    // never race a retained installation or the isolated metadata updater.
    if (check == SPUUpdateCheckUpdatesInBackground && !TSInstallRequested && !TSReplacingUpdate()) {
        if (TSRetainsInstallation() || TSProbe.completion || TSManualCheck || (TSAdmitAutomatic && !TSAdmitAutomatic(true))) {
            TSScheduledCheckSkipped = YES;
            return NO;
        }
        TSSend(9, nil);
    } else if (TSAdmitAutomatic) {
        // A user-initiated download also postpones automatic discovery, but is
        // never throttled. This does not touch signed freshness probes.
        TSAdmitAutomatic(false);
    }
    return YES;
}
// Stable items carry the "stable" channel, so builds that predate this bridge
// (which only allowed "stable") keep seeing them.
- (NSSet<NSString *> *)allowedChannelsForUpdater:(SPUUpdater *)updater {
    return TSChannels();
}
- (void)updater:(SPUUpdater *)updater didFinishLoadingAppcast:(SUAppcast *)appcast {
    TSClearAdmission();
    self.signedItems = TSSignedItems(appcast);
    self.feed = updater.feedURL.absoluteString;
    self.channels = TSChannels();
}
- (void)updater:(SPUUpdater *)updater didFindValidUpdate:(SUAppcastItem *)item {
    TSPendingAdmission = nil;
    if (self.signedItems && item.signingValidationStatus == SPUAppcastSigningValidationStatusSucceeded
        && !item.informationOnlyUpdate && !item.majorUpgrade) {
        TSAdmission *admission = [TSAdmission new];
        admission.items = self.signedItems;
        admission.item = item.propertiesDictionary;
        admission.version = item.versionString;
        admission.archive = item.fileURL;
        admission.feed = self.feed;
        admission.channels = self.channels;
        TSPendingAdmission = admission;
    }
    TSSend(1, item.displayVersionString);
}
- (void)updater:(SPUUpdater *)updater didDownloadUpdate:(SUAppcastItem *)item {
    TSDownloadedAdmission = [TSPendingAdmission covers:item feed:updater.feedURL.absoluteString
        channels:TSChannels() signedItems:self.signedItems] ? TSPendingAdmission : nil;
    TSPendingAdmission = nil;
}
- (void)updater:(SPUUpdater *)updater didFinishUpdateCycleForUpdateCheck:(SPUUpdateCheck)check error:(NSError *)error {
    // Sparkle has released its old driver and cleared sessionInProgress here.
    // Start the replacement before returning: otherwise Sparkle's automatic
    // scheduler reserves a session for interval discovery, and a queued check
    // is ignored. This is cycle completion, never a user-driver reply.
    BOOL restart = TSRestartAfterCycle && !error;
    if (TSRestartAfterCycle && error) {
        // No replacement will start: a carried-over Restart cannot continue.
        TSEndReplacement();
        TSEndRestartIntent(error.localizedDescription ?: @"The latest update could not be downloaded. Check for updates again.");
    }
    TSCancelledCycle = NO;
    TSScheduledCheckSkipped = NO;
    TSRestartAfterCycle = NO;
    if (TSCycleFailure) { TSSend(2, TSCycleFailure); TSCycleFailure = nil; }
    if (TSQuitAfterCycle) {
        // Skip/cancellation must finish its IPC cleanup before the process
        // exits, otherwise Sparkle's retained installer can install on quit.
        void (^quit)(BOOL) = TSQuitAfterCycle; TSQuitAfterCycle = nil;
        dispatch_async(dispatch_get_main_queue(), ^{ quit(YES); });
    } else if (restart) {
        if (TSReplacementForeground) [updater checkForUpdates];
        else [updater checkForUpdatesInBackground];
    }
}
- (void)updaterDidNotFindUpdate:(SPUUpdater *)updater {
    TSInstallRequested = NO;
    TSEndRestartIntent(TSNothingNewer);
    TSSend(0, nil);
}
- (void)updater:(SPUUpdater *)updater didAbortWithError:(NSError *)error {
    if (TSCancelledCycle || TSScheduledCheckSkipped) return;
    // "No update" can also arrive as an error; it is not a failure.
    if ([error.domain isEqualToString:SUSparkleErrorDomain] && error.code == SUNoUpdateError) {
        TSInstallRequested = NO;
        TSEndRestartIntent(TSNothingNewer);
        TSSend(0, nil);
    } else {
        // A replacement scheduled by this cycle keeps the restart intent.
        if (!TSRestartAfterCycle) TSEndRestartIntent(nil);
        TSSend(2, error.localizedDescription);
    }
}
@end

@interface TSTerminationDelegate : NSObject <NSApplicationDelegate>
@property(nonatomic, strong) id<NSApplicationDelegate> original;
@property(nonatomic) BOOL verifiedQuit;
@end
static TSTerminationDelegate *TSTermination;

// Public continuations only: About is the sole owner of progress and choices.
@interface TSUserDriver : TSMetadataDriver
@property(nonatomic, copy) void (^cancellation)(void);
@property(nonatomic) uint64_t expected;
@property(nonatomic) uint64_t received;
@property(nonatomic, copy) void (^restartReply)(SPUUserUpdateChoice);
@property(nonatomic, copy) void (^retryTermination)(void);
@property(nonatomic, strong) SUAppcastItem *item;
@property(nonatomic) BOOL verifying;
@property(nonatomic) BOOL verifiedRestart;
@property(nonatomic) BOOL restartRequested;
@property(nonatomic) BOOL replacing;
@property(nonatomic) BOOL preparing;
@property(nonatomic) BOOL foreground;
@property(nonatomic) NSUInteger generation;
@property(nonatomic, copy) void (^quitReply)(BOOL);
- (void)cancel;
- (void)verifyQuit:(void (^)(BOOL))reply;
- (void)requestRestart;
- (void)watchVerification:(NSUInteger)generation;
- (void)presentReady:(void (^)(SPUUserUpdateChoice))reply;
- (void)presentUpdate:(SUAppcastItem *)item state:(SPUUserUpdateState *)state reply:(void (^)(SPUUserUpdateChoice))reply;
- (void)cancelForReplacement:(void (^)(SPUUserUpdateChoice))reply error:(NSError *)error;
@end
@implementation TSUserDriver
- (void)showUserInitiatedUpdateCheckWithCancellation:(void (^)(void))cancellation {
    self.cancellation = cancellation;
    TSSend(9, nil);
}
- (void)showUpdateNotFoundWithError:(NSError *)error acknowledgement:(void (^)(void))acknowledgement {
    TSEndRestartIntent(TSNothingNewer);
    TSSend(0, nil); acknowledgement();
}
- (void)showUpdaterError:(NSError *)error acknowledgement:(void (^)(void))acknowledgement {
    if (!TSCancelledCycle && !TSScheduledCheckSkipped) {
        if (!TSRestartAfterCycle) TSEndRestartIntent(nil);
        TSSend(2, error.localizedDescription);
    }
    acknowledgement();
}
- (void)showInstallingUpdateWithApplicationTerminated:(BOOL)terminated retryTerminatingApplication:(void (^)(void))retry {
    NSAssert(NSThread.isMainThread, @"Sparkle is main-thread only");
    // Sparkle explicitly permits repeated use; each attempt still needs a new
    // user action, signed freshness and the final Rust editor guard.
    self.retryTermination = terminated ? nil : retry;
    TSSend(terminated ? 6 : 10, nil);
}
- (void)cancel {
    NSAssert(NSThread.isMainThread, @"Sparkle is main-thread only");
    void (^cancellation)(void) = self.cancellation;
    void (^ready)(SPUUserUpdateChoice) = self.restartReply;
    if (!cancellation && !ready) return;
    TSCancelledCycle = YES;
    TSInstallRequested = NO;
    TSRestartAfterCycle = NO;
    TSRestartWhenReady = NO;
    self.replacing = NO;
    [self dismissUpdateInstallation]; // invalidate in-flight freshness first
    if (ready) ready(SPUUserUpdateChoiceSkip);
    else cancellation();
    TSSend(7, nil);
}
- (void)showUpdateFoundWithAppcastItem:(SUAppcastItem *)item state:(SPUUserUpdateState *)state reply:(void (^)(SPUUserUpdateChoice))reply {
    self.cancellation = nil;
    self.item = item;
    self.foreground = state.userInitiated;
    // A pre-existing installer (from a previous build) also enters the hook.
    if (state.stage == SPUUserUpdateStageInstalling) {
        [self showReadyToInstallAndRelaunch:reply]; return;
    }
    [self presentUpdate:item state:state reply:reply];
}
- (void)presentUpdate:(SUAppcastItem *)item state:(SPUUserUpdateState *)state reply:(void (^)(SPUUserUpdateChoice))reply {
    // Background mode uses this same silent download/Ready path as an explicit
    // Download click. The Ready continuation is never consumed automatically.
    if ((self.replacing || TSInstallRequested || TSBackgroundDownloads || TSRestartWhenReady) && !item.informationOnlyUpdate && !item.majorUpgrade
        && item.signingValidationStatus != SPUAppcastSigningValidationStatusFailed) {
        self.replacing = NO;
        TSInstallRequested = NO;
        reply(SPUUserUpdateChoiceInstall);
    } else {
        self.replacing = NO;
        TSInstallRequested = NO;
        if (item.informationOnlyUpdate || item.majorUpgrade || item.signingValidationStatus == SPUAppcastSigningValidationStatusFailed) {
            TSEndRestartIntent(nil);
            TSSend(2, @"This update requires a separate upgrade or cannot be verified. Nothing was installed.");
        }
        reply(SPUUserUpdateChoiceDismiss);
    }
}
- (void)cancelForReplacement:(void (^)(SPUUserUpdateChoice))reply error:(NSError *)error {
    // Superseded (no error): a newer update replaces this one. A Restart the
    // user already chose carries over and continues when the newest update is
    // verified (#2679); the single notice says so. An error ends the attempt.
    BOOL intent = self.restartRequested || TSRestartWhenReady;
    if (intent && error) {
        TSRestartWhenReady = NO;
        TSSend(14, error.localizedDescription);
    } else if (intent) {
        TSRestartWhenReady = YES;
        TSSend(13, TSDownloadingLatest);
    }
    self.restartRequested = NO;
    self.restartReply = nil;
    self.retryTermination = nil;
    self.verifiedRestart = NO;
    TSGuardRequested = NO;
    if (self.quitReply) { void (^quit)(BOOL) = self.quitReply; self.quitReply = nil; quit(NO); }
    TSRestartAfterCycle = error == nil;
    TSReplacementForeground = self.foreground;
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
    self.cancellation = cancellation;
}
- (void)showDownloadDidReceiveExpectedContentLength:(uint64_t)length {
    self.expected = length;
}
- (void)showDownloadDidReceiveDataOfLength:(uint64_t)length {
    self.received += length;
    if (self.expected) TSSend(3, [NSString stringWithFormat:@"%u", (unsigned)MIN(100.0, 100.0 * self.received / self.expected)]);
}
- (void)showDownloadDidStartExtractingUpdate {
    self.preparing = YES;
    TSSend(4, nil);
    self.cancellation = nil;
}
- (void)showReadyToInstallAndRelaunch:(void (^)(SPUUserUpdateChoice))reply {
    self.restartReply = reply;
    if (TSQuitAfterCycle) {
        // Extraction has no cancellation callback. At Ready, Skip the installer
        // and let cycle completion release the pending ordinary Quit.
        [self cancel]; return;
    }
    self.verifying = YES;
    NSUInteger generation = ++self.generation;
    TSSend(4, nil);
    __weak TSUserDriver *weakSelf = self;
    TSCheckFreshness(self.item, self.foreground, ^(BOOL current, NSError *error) {
        TSUserDriver *driver = weakSelf;
        if (!driver || driver.generation != generation) return;
        driver.verifying = NO;
        if (!current) { [driver cancelForReplacement:reply error:error]; return; }
        driver.restartReply = reply;
        if (driver.quitReply) {
            driver.verifiedRestart = YES;
            TSGuardRequested = YES;
            TSSend(5, driver.item.versionString);
            return;
        }
        TSSend(5, driver.item.versionString);
        if (driver.restartRequested || TSRestartWhenReady) {
            TSRestartWhenReady = NO;
            [driver requestRestart]; return;
        }
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
    [self watchVerification:generation];
}
- (void)presentReady:(void (^)(SPUUserUpdateChoice))reply {
    // Rust About retains the Ready state; Restart now calls requestRestart.
}
- (void)requestRestart {
    NSAssert(NSThread.isMainThread, @"Sparkle is main-thread only");
    if ((!self.restartReply && !self.retryTermination) || self.verifying || TSGuardRequested) return;
    self.verifying = YES;
    self.restartRequested = YES;
    self.verifiedRestart = NO;
    TSGuardRequested = NO;
    NSUInteger generation = ++self.generation;
    TSSend(4, nil);
    __weak TSUserDriver *weakSelf = self;
    TSCheckFreshness(self.item, self.foreground, ^(BOOL current, NSError *error) {
        TSUserDriver *driver = weakSelf;
        if (!driver || driver.generation != generation) return;
        driver.verifying = NO;
        if (!driver.restartReply && !driver.retryTermination) return;
        if (!current) {
            if (driver.restartReply) [driver cancelForReplacement:driver.restartReply error:error];
            else {
                if (driver.quitReply) { void (^quit)(BOOL) = driver.quitReply; driver.quitReply = nil; quit(NO); }
                TSSend(10, error.localizedDescription ?: @"The feed changed. This installation cannot restart until its freshness is verified.");
            }
            return;
        }
        driver.verifiedRestart = YES;
        TSGuardRequested = YES;
        // Rust reruns editor protection/UI persistence after the async check,
        // for both ordinary Quit and Okilum's Restart action.
        TSSend(5, driver.item.versionString);
    });
    [self watchVerification:generation];
}
- (void)watchVerification:(NSUInteger)generation {
    __weak TSUserDriver *weakSelf = self;
    // A stalled network request must not leave an apparently dead Restart
    // button. Invalidate this attempt; a late fetch cannot authorize install.
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 15 * NSEC_PER_SEC), dispatch_get_main_queue(), ^{
        TSUserDriver *driver = weakSelf;
        if (!driver || driver.generation != generation || !driver.verifying) return;
        driver.generation += 1;
        driver.verifying = NO;
        driver.restartRequested = NO;
        TSRestartWhenReady = NO;
        driver.verifiedRestart = NO;
        TSGuardRequested = NO;
        if (driver.quitReply) { void (^quit)(BOOL) = driver.quitReply; driver.quitReply = nil; quit(NO); }
        TSSend(10, @"Update verification is taking too long. Check your connection, then choose Restart now again. Nothing was installed.");
    });
}
- (void)verifyQuit:(void (^)(BOOL))reply {
    // A refused termination after a prior explicit Restart is already an
    // authorized installation transaction. Preserve its freshness/editor guard.
    if (self.retryTermination) {
        self.quitReply = reply;
        if (!self.verifying) [self requestRestart];
        return;
    }
    // Ordinary Quit is never initial installation consent. Wait for Sparkle's
    // cancellation cycle, including helper IPC, before forwarding termination.
    TSQuitAfterCycle = reply;
    if (self.cancellation || self.restartReply) [self cancel];
}
- (void)showUpdateInstalledAndRelaunched:(BOOL)relaunched acknowledgement:(void (^)(void))acknowledgement {
    [self dismissUpdateInstallation];
    if (relaunched) TSSend(8, nil);
    acknowledgement();
}
- (void)dismissUpdateInstallation {
    NSAssert(NSThread.isMainThread, @"Sparkle is main-thread only");
    TSClearAdmission();
    self.cancellation = nil;
    self.retryTermination = nil;
    TSTermination.verifiedQuit = NO;
    TSInstallRequested = NO;
    self.generation += 1;
    self.preparing = NO;
    self.verifying = NO;
    if (self.quitReply) { void (^quit)(BOOL) = self.quitReply; self.quitReply = nil; quit(NO); }
    self.restartReply = nil;
    self.verifiedRestart = NO;
    self.restartRequested = NO;
    TSGuardRequested = NO;
    [super dismissUpdateInstallation];
}
@end
// Sparkle can install a prepared interactive update on ordinary Quit. Cancel
// that session first, then forward shutdown to GPUI without installation.
// Forward every other delegate callback unchanged; no GPUI/source patch.
@implementation TSTerminationDelegate
- (BOOL)respondsToSelector:(SEL)selector {
    return [super respondsToSelector:selector] || [self.original respondsToSelector:selector];
}
- (id)forwardingTargetForSelector:(SEL)selector { return self.original; }
- (void)applicationWillTerminate:(NSNotification *)notification {
    // GPUI reads its original delegate's native ivar after NSApp.run returns.
    // Restore that exact object before forwarding shutdown; never impersonate
    // or copy GPUI's private native state into this adapter.
    [TSDriver dismissUpdateInstallation];
    NSApp.delegate = self.original;
    if ([self.original respondsToSelector:_cmd]) [self.original applicationWillTerminate:notification];
}
- (NSApplicationTerminateReply)applicationShouldTerminate:(NSApplication *)app {
    // Repeated Quit cannot bypass helper cleanup already in progress.
    if (TSQuitAfterCycle) return NSTerminateCancel;
    if (self.verifiedQuit || (!TSDriver.restartReply && !TSDriver.retryTermination && !TSDriver.preparing && !TSCancelledCycle)) {
        // Consume approval once, even if the original delegate refuses Quit.
        // Its own NSTerminateLater/reply contract remains its responsibility.
        self.verifiedQuit = NO;
        return [self.original respondsToSelector:_cmd] ? [self.original applicationShouldTerminate:app] : NSTerminateNow;
    }
    [TSDriver verifyQuit:^(BOOL current) {
        if (!current) return;
        // The final Rust editor guard queues this callback outside its GPUI
        // borrow. Reissue ordinary Quit; never request Sparkle's relaunch path.
        self.verifiedQuit = YES;
        [app terminate:nil];
    }];
    return NSTerminateCancel;
}
@end
static TSUpdaterDelegate *TSDelegate;

static BOOL TSReplacingUpdate(void) { return TSDriver.replacing; }
static void TSEndReplacement(void) { TSDriver.replacing = NO; }
static BOOL TSRetainsInstallation(void) { return TSDriver.restartReply || TSDriver.retryTermination || TSDriver.preparing || TSDriver.verifying || TSGuardRequested; }

static void TSStart(NSBundle *bundle, TSUserDriver *driver, TSUpdaterDelegate *delegate) {
    TSTermination = [TSTerminationDelegate new];
    TSTermination.original = NSApp.delegate;
    NSApp.delegate = TSTermination;
    TSProbe = [TSFreshnessProbe new];
    TSDelegate = delegate ?: [TSUpdaterDelegate new];
    TSDriver = driver;
    TSUpdater = [[SPUUpdater alloc] initWithHostBundle:bundle
                                 applicationBundle:bundle
                                        userDriver:TSDriver delegate:TSDelegate];
    // Public setting, including existing installs with an older saved value.
    // Stock Sparkle 2.10 clamps scheduling to >=1 hour. Rust also drives silent
    // metadata discovery every 28-32 minutes, without patching that minimum.
    TSUpdater.automaticallyChecksForUpdates = TSAutomaticChecks;
    TSUpdater.automaticallyDownloadsUpdates = NO; // use the guarded silent driver
    TSUpdater.updateCheckInterval = 28 * 60 + arc4random_uniform(4 * 60 + 1);
    NSError *error = nil;
    if (![TSUpdater startUpdater:&error]) TSSend(2, error.localizedDescription);
}
void okilum_updater_start(void) {
    TSRegisterChannelDefault();
    // A bare `cargo run` binary has no bundle feed; Sparkle would only report errors.
    if (TSUpdater || ![NSBundle.mainBundle objectForInfoDictionaryKey:@"SUFeedURL"]) return;
    TSStart(NSBundle.mainBundle, [TSUserDriver new], nil);
    [NSNotificationCenter.defaultCenter addObserverForName:NSApplicationDidBecomeActiveNotification
        object:NSApp queue:nil usingBlock:^(NSNotification *notification) { TSSend(12, nil); }];
}

bool okilum_updater_available(void) { return TSUpdater != nil; }

void okilum_updater_set_report(TSReport report) { TSReporter = report; }
void okilum_updater_set_automatic_admission(TSAutomaticAdmission admit) { TSAdmitAutomatic = admit; }

// False while a check or download session is already running: Sparkle would
// ignore the probe and never call the delegate.
bool okilum_updater_check(bool download, bool foreground) {
    // Sparkle's session flag describes its current driver, not ownership of our
    // retained Ready/retry continuations. Discovery must never replace them.
    if (!TSUpdater || TSDriver.restartReply || TSDriver.retryTermination || TSDriver.preparing
        || TSDriver.verifying || TSGuardRequested || TSUpdater.sessionInProgress || TSProbe.completion || TSManualCheck) return false;
    TSManualCheck = YES;
    dispatch_async(dispatch_get_main_queue(), ^{
        [TSProbe check:TSUpdater channels:TSChannels() foreground:NO completion:^(SUAppcastItem *selected, NSError *error) {
            TSManualCheck = NO;
            if (error && !([error.domain isEqualToString:SUSparkleErrorDomain] && error.code == SUNoUpdateError)) {
                TSEndRestartIntent(nil);
                TSSend(2, error.localizedDescription);
            }
            else if (selected) {
                TSSend(1, selected.displayVersionString);
                // A Restart chosen during this check downloads the newest update.
                if (download || TSRestartWhenReady) {
                    TSInstallRequested = YES;
                    if (foreground) [TSUpdater checkForUpdates];
                    else [TSUpdater checkForUpdatesInBackground];
                }
            }
            else { TSEndRestartIntent(TSNothingNewer); TSSend(0, nil); }
        }];
    });
    return true;
}

// Native scheduling obeys the same durable Rust preference. Sparkle's own
// automatic installer stays disabled: our silent driver stops at Ready.
void okilum_updater_set_mode(int mode) {
    TSBackgroundDownloads = mode == 0;
    TSAutomaticChecks = mode != 2;
    TSUpdater.automaticallyChecksForUpdates = TSAutomaticChecks;
    TSUpdater.automaticallyDownloadsUpdates = NO;
}
bool okilum_updater_can_cancel(void) { return TSDriver.cancellation != nil || TSDriver.restartReply != nil; }
void okilum_updater_cancel(void) { [TSDriver cancel]; }

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
    if (!TSUpdater) return false;
    if (!TSDriver.restartReply && !TSDriver.retryTermination) {
        // #2679: the notice the user clicked predates Sparkle's current state;
        // a newer update superseded it. Never answer «no longer ready»: keep
        // the consent and restart into the newest update once it is verified.
        TSRestartWhenReady = YES;
        TSSend(13, TSDownloadingLatest);
        // A running check, download or replacement continues by itself.
        if (TSDriver.replacing || TSDriver.preparing || TSRestartAfterCycle || TSUpdater.sessionInProgress
            || TSProbe.completion || TSManualCheck) return false;
        // Nothing is running: fetch the newest update now.
        TSInstallRequested = YES;
        [TSUpdater checkForUpdatesInBackground];
        return false;
    }
    if (TSDriver.verifying) {
        // A toast may have been drawn while initial Ready verification was
        // still running. Retain this explicit consent instead of losing it.
        TSDriver.restartRequested = YES;
        TSSend(13, @"The update is being verified. Restart will continue automatically when verification finishes.");
        return false;
    }
    if (TSGuardRequested) {
        TSSend(13, @"Restart is protecting your edits and saving the window state. It will continue automatically.");
        return false;
    }
    if (TSProbe.completion || TSManualCheck) {
        TSSend(14, @"An update check is still running. Wait for it to finish, then choose Restart now again.");
        return false;
    }
    if (!TSGuardRequested) [TSDriver requestRestart];
    return true;
}
// Separate entry point: only Rust's post-verification editor/save guards may
// consume the install continuation. Repeated UI clicks cannot bypass them.
bool okilum_updater_finish_restart(void) {
    NSCAssert(NSThread.isMainThread, @"Sparkle is main-thread only");
    if ((!TSDriver.restartReply && !TSDriver.retryTermination) || TSDriver.verifying || !TSDriver.verifiedRestart || !TSGuardRequested) return false;
    TSDriver.verifiedRestart = NO;
    TSGuardRequested = NO;
    if (TSDriver.quitReply) {
        // The same Rust protection/persistence guard has just succeeded. Only
        // now may AppKit accept ordinary Quit; do not request a relaunch.
        void (^quit)(BOOL) = TSDriver.quitReply;
        TSDriver.quitReply = nil;
        // Release GPUI's mutable App borrow before AppKit synchronously sends
        // applicationWillTerminate (the same ordering as GPUI's normal Quit).
        dispatch_async(dispatch_get_main_queue(), ^{ quit(YES); });
        return true;
    }
    if (TSDriver.retryTermination) {
        void (^retry)(void) = TSDriver.retryTermination;
        TSTermination.verifiedQuit = YES; // one original-delegate decision per guarded attempt
        TSSend(6, nil);
        NSUInteger generation = TSDriver.generation;
        retry(); // public Sparkle continuation, never NSApp terminate directly
        // The callback is reusable and need not be delivered again after a
        // refused quit. Keep About actionable while the application is alive.
        if (TSDriver.generation == generation && TSDriver.retryTermination) TSSend(10, nil);
        return true;
    }
    TSDriver.preparing = NO;
    void (^continuation)(SPUUserUpdateChoice) = TSDriver.restartReply;
    TSDriver.restartReply = nil;
    // Clear retained handlers before consuming the verified continuation.
    [TSDriver dismissUpdateInstallation];
    TSTermination.verifiedQuit = YES;
    TSSend(6, nil);
    continuation(SPUUserUpdateChoiceInstall);
    return true;
}
bool okilum_updater_ready(void) { return TSDriver.restartReply != nil || TSDriver.retryTermination != nil; }

// Polled on the GPUI main thread; the guard runs only after freshness succeeds.
bool okilum_updater_guard_requested(void) { return TSGuardRequested; }
// Ordinary AppKit Quit uses the same save guard but does not relaunch.
bool okilum_updater_restart_relaunches(void) { return TSDriver.quitReply == nil; }
void okilum_updater_refuse_restart(void) {
    TSGuardRequested = NO;
    if (TSDriver.restartReply) {
        NSError *error = [NSError errorWithDomain:@"OkilumUpdate" code:4 userInfo:@{NSLocalizedDescriptionKey:@"Your edits could not be protected. Nothing was installed."}];
        [TSDriver cancelForReplacement:TSDriver.restartReply error:error];
    } else if (TSDriver.retryTermination) {
        TSDriver.verifiedRestart = NO;
        if (TSDriver.quitReply) { void (^quit)(BOOL) = TSDriver.quitReply; TSDriver.quitReply = nil; quit(NO); }
        TSSend(10, @"Your latest edits could not be protected. Okilum will stay open. Try Restart now again after resolving the error.");
    }
}

// Public Sparkle user-driver adapter: inline manual-check outcomes, stock install UI.
#import <AppKit/AppKit.h>
#import <Sparkle/Sparkle.h>
#include <stdbool.h>
#include <stdint.h>

static NSString *const TSBetaKey = @"TesseraReceiveBetaBuilds";
// Stable ABI with updater/check_status.rs. Sparkle and Rust call this on the main thread.
typedef NS_ENUM(uint32_t, TSCheckStatus) {
    TSIdle, TSChecking, TSUpToDate, TSNewerVersion, TSNewerOSRequired,
    TSOlderOSRequired, TSUnsupportedHardware, TSNoCompatibleUpdate,
    TSCheckFailed, TSUpdateAvailable, TSBusy
};
static TSCheckStatus TSStatus;

@interface TSUpdaterDelegate : NSObject <SPUUpdaterDelegate>
@end
@implementation TSUpdaterDelegate
- (NSSet<NSString *> *)allowedChannelsForUpdater:(SPUUpdater *)updater {
    return [NSUserDefaults.standardUserDefaults boolForKey:TSBetaKey]
        ? [NSSet setWithObjects:@"stable", @"beta", nil]
        : [NSSet setWithObject:@"stable"];
}
@end

@interface TSUserDriver : NSObject <SPUUserDriver>
@property(nonatomic, strong) SPUStandardUserDriver *standard;
@property(nonatomic) BOOL manualCheck;
@property(nonatomic) BOOL standardActive;
@end
@implementation TSUserDriver
- (void)showUserInitiatedUpdateCheckWithCancellation:(void (^)(void))cancellation {
    self.manualCheck = YES;
    TSStatus = TSChecking;
    // No progress window: the explicit check lives in Settings → Updates.
}
- (void)showUpdateNotFoundWithError:(NSError *)error acknowledgement:(void (^)(void))acknowledgement {
    if (!self.manualCheck) {
        [self.standard showUpdateNotFoundWithError:error acknowledgement:acknowledgement];
        return;
    }
    self.manualCheck = NO;
    NSNumber *reason = error.userInfo[SPUNoUpdateFoundReasonKey];
    switch (reason.integerValue) {
        case SPUNoUpdateFoundReasonOnLatestVersion: TSStatus = TSUpToDate; break;
        case SPUNoUpdateFoundReasonOnNewerThanLatestVersion: TSStatus = TSNewerVersion; break;
        case SPUNoUpdateFoundReasonSystemIsTooOld: TSStatus = TSNewerOSRequired; break;
        case SPUNoUpdateFoundReasonSystemIsTooNew: TSStatus = TSOlderOSRequired; break;
        case SPUNoUpdateFoundReasonHardwareDoesNotSupportARM64: TSStatus = TSUnsupportedHardware; break;
        default: TSStatus = TSNoCompatibleUpdate; break;
    }
    acknowledgement();
}
- (void)showUpdaterError:(NSError *)error acknowledgement:(void (^)(void))acknowledgement {
    if (!self.manualCheck) {
        // Download/install errors stay with their visible standard workflow.
        [self.standard showUpdaterError:error acknowledgement:acknowledgement];
        return;
    }
    self.manualCheck = NO;
    TSStatus = TSCheckFailed;
    acknowledgement();
}
- (void)showUpdateFoundWithAppcastItem:(SUAppcastItem *)item state:(SPUUserUpdateState *)state reply:(void (^)(SPUUserUpdateChoice))reply {
    if (self.manualCheck) TSStatus = TSUpdateAvailable;
    self.manualCheck = NO;
    self.standardActive = YES;
    [self.standard showUpdateFoundWithAppcastItem:item state:state reply:reply];
}
- (void)showUpdatePermissionRequest:(SPUUpdatePermissionRequest *)request reply:(void (^)(SUUpdatePermissionResponse *))reply {
    self.standardActive = YES;
    __weak TSUserDriver *weakSelf = self;
    [self.standard showUpdatePermissionRequest:request reply:^(SUUpdatePermissionResponse *response) {
        weakSelf.standardActive = NO;
        reply(response);
    }];
}
- (void)showUpdateReleaseNotesWithDownloadData:(SPUDownloadData *)data {
    [self.standard showUpdateReleaseNotesWithDownloadData:data];
}
- (void)showUpdateReleaseNotesFailedToDownloadWithError:(NSError *)error {
    [self.standard showUpdateReleaseNotesFailedToDownloadWithError:error];
}
- (void)showDownloadInitiatedWithCancellation:(void (^)(void))cancellation {
    self.standardActive = YES;
    [self.standard showDownloadInitiatedWithCancellation:cancellation];
}
- (void)showDownloadDidReceiveExpectedContentLength:(uint64_t)length {
    [self.standard showDownloadDidReceiveExpectedContentLength:length];
}
- (void)showDownloadDidReceiveDataOfLength:(uint64_t)length {
    [self.standard showDownloadDidReceiveDataOfLength:length];
}
- (void)showDownloadDidStartExtractingUpdate {
    [self.standard showDownloadDidStartExtractingUpdate];
}
- (void)showExtractionReceivedProgress:(double)progress {
    [self.standard showExtractionReceivedProgress:progress];
}
- (void)showReadyToInstallAndRelaunch:(void (^)(SPUUserUpdateChoice))reply {
    self.standardActive = YES;
    [self.standard showReadyToInstallAndRelaunch:reply];
}
- (void)showInstallingUpdateWithApplicationTerminated:(BOOL)terminated retryTerminatingApplication:(void (^)(void))retry {
    self.standardActive = YES;
    [self.standard showInstallingUpdateWithApplicationTerminated:terminated retryTerminatingApplication:retry];
}
- (void)showUpdateInstalledAndRelaunched:(BOOL)relaunched acknowledgement:(void (^)(void))acknowledgement {
    [self.standard showUpdateInstalledAndRelaunched:relaunched acknowledgement:acknowledgement];
}
- (void)dismissUpdateInstallation {
    if (self.manualCheck || TSStatus == TSUpdateAvailable) TSStatus = TSIdle;
    self.manualCheck = NO;
    self.standardActive = NO;
    [self.standard dismissUpdateInstallation];
}
- (void)showUpdateInFocus {
    [self.standard showUpdateInFocus];
}
@end

static SPUUpdater *TSUpdater;
static TSUpdaterDelegate *TSDelegate;
static TSUserDriver *TSDriver;
static BOOL TSStarted;

void tessera_updater_start(void) {
    [NSUserDefaults.standardUserDefaults registerDefaults:@{TSBetaKey : @YES}];
    if (TSUpdater || ![NSBundle.mainBundle objectForInfoDictionaryKey:@"SUFeedURL"]) return;
    TSDelegate = [TSUpdaterDelegate new];
    TSDriver = [TSUserDriver new];
    TSDriver.standard = [[SPUStandardUserDriver alloc] initWithHostBundle:NSBundle.mainBundle delegate:nil];
    TSUpdater = [[SPUUpdater alloc] initWithHostBundle:NSBundle.mainBundle
                                  applicationBundle:NSBundle.mainBundle
                                         userDriver:TSDriver delegate:TSDelegate];
    NSError *error = nil;
    TSStarted = [TSUpdater startUpdater:&error];
    if (!TSStarted) TSStatus = TSCheckFailed;
}

bool tessera_updater_available(void) { return TSStarted; }
uint32_t tessera_updater_status(void) {
    if (TSStatus == TSBusy && !TSDriver.standardActive && TSUpdater.canCheckForUpdates) TSStatus = TSIdle;
    return TSStatus;
}
bool tessera_updater_check(void) {
    if (!TSStarted || TSDriver.manualCheck) return false;
    if (TSDriver.standardActive) {
        if (TSStatus != TSUpdateAvailable) TSStatus = TSBusy;
        [TSDriver showUpdateInFocus];
        return false;
    }
    if (!TSUpdater.canCheckForUpdates) {
        TSStatus = TSBusy;
        [TSDriver showUpdateInFocus];
        return false;
    }
    TSDriver.manualCheck = YES;
    TSStatus = TSChecking;
    [TSUpdater checkForUpdates];
    return true;
}

bool tessera_updater_beta(void) {
    [NSUserDefaults.standardUserDefaults registerDefaults:@{TSBetaKey : @YES}];
    return [NSUserDefaults.standardUserDefaults boolForKey:TSBetaKey];
}
void tessera_updater_set_beta(bool enabled) {
    [NSUserDefaults.standardUserDefaults setBool:enabled forKey:TSBetaKey];
}

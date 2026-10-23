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

@interface TSUpdaterDelegate : NSObject <SPUUpdaterDelegate>
@end
@implementation TSUpdaterDelegate
// Stable items carry the "stable" channel, so builds that predate this bridge
// (which only allowed "stable") keep seeing them.
- (NSSet<NSString *> *)allowedChannelsForUpdater:(SPUUpdater *)updater {
    return [NSUserDefaults.standardUserDefaults boolForKey:TSBetaKey]
        ? [NSSet setWithObjects:@"stable", @"beta", nil]
        : [NSSet setWithObject:@"stable"];
}
- (void)updater:(SPUUpdater *)updater didFindValidUpdate:(SUAppcastItem *)item {
    TSSend(1, item.displayVersionString);
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

// Extend the stock driver only to mirror progress and retain Sparkle's public
// install-and-relaunch continuation. All prompts/errors remain Sparkle-owned.
@interface TSUserDriver : SPUStandardUserDriver
@property(nonatomic) uint64_t expected;
@property(nonatomic) uint64_t received;
@property(nonatomic, copy) void (^restartReply)(SPUUserUpdateChoice);
@end
@implementation TSUserDriver
- (void)showDownloadInitiatedWithCancellation:(void (^)(void))cancellation {
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
    TSSend(4, nil);
    [super showDownloadDidStartExtractingUpdate];
}
- (void)showReadyToInstallAndRelaunch:(void (^)(SPUUserUpdateChoice))reply {
    // Do not fire this continuation until the user explicitly chooses Restart
    // and Rust protects editors/persists UI state. Either UI invokes the same
    // retained continuation once, never merely quits the application.
    self.restartReply = reply;
    TSSend(5, nil);
    __weak TSUserDriver *weakSelf = self;
    [super showReadyToInstallAndRelaunch:^(SPUUserUpdateChoice choice) {
        TSUserDriver *driver = weakSelf;
        if (!driver.restartReply) return;
        // Stock Sparkle's own dialog still uses its normal termination guards.
        void (^continuation)(SPUUserUpdateChoice) = driver.restartReply;
        driver.restartReply = nil;
        TSSend(choice == SPUUserUpdateChoiceInstall ? 6 : 7, nil);
        continuation(choice);
    }];
}
- (void)showUpdateInstalledAndRelaunched:(BOOL)relaunched acknowledgement:(void (^)(void))acknowledgement {
    if (relaunched) TSSend(8, nil);
    [super showUpdateInstalledAndRelaunched:relaunched acknowledgement:acknowledgement];
}
- (void)dismissUpdateInstallation {
    self.restartReply = nil;
    [super dismissUpdateInstallation];
}
@end
static SPUUpdater *TSUpdater;
static TSUserDriver *TSDriver;
static TSUpdaterDelegate *TSDelegate;

void okilum_updater_start(void) {
    TSRegisterChannelDefault();
    // A bare `cargo run` binary has no bundle feed; Sparkle would only report errors.
    if (TSUpdater || ![NSBundle.mainBundle objectForInfoDictionaryKey:@"SUFeedURL"]) return;
    TSDelegate = [TSUpdaterDelegate new];
    TSDriver = [[TSUserDriver alloc] initWithHostBundle:NSBundle.mainBundle delegate:nil];
    TSUpdater = [[SPUUpdater alloc] initWithHostBundle:NSBundle.mainBundle
                                 applicationBundle:NSBundle.mainBundle
                                        userDriver:TSDriver delegate:TSDelegate];
    NSError *error = nil;
    if (![TSUpdater startUpdater:&error]) TSSend(2, error.localizedDescription);
}

bool okilum_updater_available(void) { return TSUpdater != nil; }

void okilum_updater_set_report(TSReport report) { TSReporter = report; }

// False while a check or download session is already running: Sparkle would
// ignore the probe and never call the delegate.
bool okilum_updater_check(void) {
    if (TSUpdater.sessionInProgress) return false;
    [TSUpdater checkForUpdateInformation];
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
    if (!TSDriver.restartReply) return false;
    void (^continuation)(SPUUserUpdateChoice) = TSDriver.restartReply;
    TSDriver.restartReply = nil;
    TSSend(6, nil);
    continuation(SPUUserUpdateChoiceInstall);
    return true;
}
bool okilum_updater_ready(void) { return TSDriver.restartReply != nil; }

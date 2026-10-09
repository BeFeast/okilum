// Stock Sparkle 2: standard updater controller, public API only. A manual
// check only probes the feed and reports the outcome to Rust, shown inline
// (#995); Sparkle's own UI appears only to install a found update.
#import <AppKit/AppKit.h>
#import <Sparkle/Sparkle.h>
#include <stdbool.h>

static NSString *const TSBetaKey = @"OkilumReceiveBetaBuilds";

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

static SPUStandardUpdaterController *TSController;
static TSUpdaterDelegate *TSDelegate;

void okilum_updater_start(void) {
    [NSUserDefaults.standardUserDefaults registerDefaults:@{TSBetaKey : @YES}];
    // A bare `cargo run` binary has no bundle feed; Sparkle would only report errors.
    if (TSController || ![NSBundle.mainBundle objectForInfoDictionaryKey:@"SUFeedURL"]) return;
    TSDelegate = [TSUpdaterDelegate new];
    TSController = [[SPUStandardUpdaterController alloc] initWithStartingUpdater:YES
                                                                 updaterDelegate:TSDelegate
                                                              userDriverDelegate:nil];
}

bool okilum_updater_available(void) { return TSController != nil; }

void okilum_updater_set_report(TSReport report) { TSReporter = report; }

void okilum_updater_check(void) { [TSController.updater checkForUpdateInformation]; }

// Sparkle's standard UI downloads, installs and relaunches.
void okilum_updater_install(void) { [TSController checkForUpdates:nil]; }

bool okilum_updater_beta(void) {
    [NSUserDefaults.standardUserDefaults registerDefaults:@{TSBetaKey : @YES}];
    return [NSUserDefaults.standardUserDefaults boolForKey:TSBetaKey];
}

void okilum_updater_set_beta(bool enabled) {
    [NSUserDefaults.standardUserDefaults setBool:enabled forKey:TSBetaKey];
}

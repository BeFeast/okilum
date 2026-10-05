// Stock Sparkle 2: standard updater controller and UI, public API only.
#import <AppKit/AppKit.h>
#import <Sparkle/Sparkle.h>
#include <stdbool.h>

static NSString *const TSBetaKey = @"TesseraReceiveBetaBuilds";

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
@end

static SPUStandardUpdaterController *TSController;
static TSUpdaterDelegate *TSDelegate;

void tessera_updater_start(void) {
    [NSUserDefaults.standardUserDefaults registerDefaults:@{TSBetaKey : @YES}];
    // A bare `cargo run` binary has no bundle feed; Sparkle would only report errors.
    if (TSController || ![NSBundle.mainBundle objectForInfoDictionaryKey:@"SUFeedURL"]) return;
    TSDelegate = [TSUpdaterDelegate new];
    TSController = [[SPUStandardUpdaterController alloc] initWithStartingUpdater:YES
                                                                 updaterDelegate:TSDelegate
                                                              userDriverDelegate:nil];
}

bool tessera_updater_available(void) { return TSController != nil; }

void tessera_updater_check(void) { [TSController checkForUpdates:nil]; }

bool tessera_updater_beta(void) {
    [NSUserDefaults.standardUserDefaults registerDefaults:@{TSBetaKey : @YES}];
    return [NSUserDefaults.standardUserDefaults boolForKey:TSBetaKey];
}

void tessera_updater_set_beta(bool enabled) {
    [NSUserDefaults.standardUserDefaults setBool:enabled forKey:TSBetaKey];
}

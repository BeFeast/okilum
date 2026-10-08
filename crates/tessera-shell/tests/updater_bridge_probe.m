// Native macOS protocol probe. Build/run command is in specs/750-inline-update-status.md.
#import "../src/updater/bridge.m"
#include <assert.h>

@interface TSRecordingDriver : SPUStandardUserDriver
@property(nonatomic) NSUInteger notFound;
@property(nonatomic) NSUInteger errors;
@property(nonatomic) NSUInteger found;
@property(nonatomic) NSUInteger dismissals;
@end
@implementation TSRecordingDriver
- (void)showUpdateNotFoundWithError:(NSError *)error acknowledgement:(void (^)(void))ack {
    self.notFound++;
    ack();
}
- (void)showUpdaterError:(NSError *)error acknowledgement:(void (^)(void))ack {
    self.errors++;
    ack();
}
- (void)showUpdateFoundWithAppcastItem:(SUAppcastItem *)item state:(SPUUserUpdateState *)state reply:(void (^)(SPUUserUpdateChoice))reply {
    self.found++;
    reply(SPUUserUpdateChoiceDismiss);
}
- (void)dismissUpdateInstallation { self.dismissals++; }
@end

int main(void) {
    @autoreleasepool {
        TSRecordingDriver *standard = [[TSRecordingDriver alloc] initWithHostBundle:NSBundle.mainBundle delegate:nil];
        TSUserDriver *driver = [TSUserDriver new];
        driver.standard = standard;
        __block NSUInteger acknowledgements = 0;
        void (^ack)(void) = ^{ acknowledgements++; };
        const SPUNoUpdateFoundReason reasons[] = {
            SPUNoUpdateFoundReasonOnLatestVersion, SPUNoUpdateFoundReasonOnNewerThanLatestVersion,
            SPUNoUpdateFoundReasonSystemIsTooOld, SPUNoUpdateFoundReasonSystemIsTooNew,
            SPUNoUpdateFoundReasonHardwareDoesNotSupportARM64, SPUNoUpdateFoundReasonUnknown
        };
        const TSCheckStatus expected[] = {TSUpToDate, TSNewerVersion, TSNewerOSRequired,
            TSOlderOSRequired, TSUnsupportedHardware, TSNoCompatibleUpdate};
        for (NSUInteger i = 0; i < sizeof(reasons) / sizeof(reasons[0]); i++) {
            [driver showUserInitiatedUpdateCheckWithCancellation:^{}];
            assert(TSStatus == TSChecking && driver.manualCheck);
            NSError *error = [NSError errorWithDomain:SUSparkleErrorDomain code:SUNoUpdateError
                userInfo:@{SPUNoUpdateFoundReasonKey: @(reasons[i])}];
            NSUInteger before = acknowledgements;
            [driver showUpdateNotFoundWithError:error acknowledgement:ack];
            assert(acknowledgements == before + 1 && TSStatus == expected[i]);
            assert(!driver.manualCheck && standard.notFound == 0);
            [driver dismissUpdateInstallation];
            assert(TSStatus == expected[i]);
        }
        NSError *error = [NSError errorWithDomain:NSURLErrorDomain code:NSURLErrorNotConnectedToInternet userInfo:nil];
        [driver showUserInitiatedUpdateCheckWithCancellation:^{}];
        NSUInteger before = acknowledgements;
        [driver showUpdaterError:error acknowledgement:ack];
        assert(TSStatus == TSCheckFailed && acknowledgements == before + 1 && standard.errors == 0);
        [driver showUserInitiatedUpdateCheckWithCancellation:^{}];
        __block BOOL replied = NO;
        // Sentinel instances are only forwarded; the recording driver never reads them.
        [driver showUpdateFoundWithAppcastItem:(SUAppcastItem *)(id)[NSObject new]
            state:(SPUUserUpdateState *)(id)[NSObject new] reply:^(SPUUserUpdateChoice choice) { replied = YES; }];
        assert(standard.found == 1 && replied && !driver.manualCheck && TSStatus == TSUpdateAvailable);
        before = acknowledgements;
        [driver showUpdaterError:error acknowledgement:ack];
        assert(standard.errors == 1 && acknowledgements == before + 1);
        [driver dismissUpdateInstallation];
        assert(TSStatus == TSIdle);
        // Positive control: background results still reach the stock driver.
        [driver showUpdateNotFoundWithError:error acknowledgement:ack];
        assert(standard.notFound == 1);
        puts("PASS: manual results inline; terminal acknowledgement once; install/background forwarding intact");
    }
    return 0;
}

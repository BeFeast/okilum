#import <AppKit/AppKit.h>

typedef void (*OkilumFolderOpen)(const char *url);
static OkilumFolderOpen folderOpen;

@interface OkilumFolderService : NSObject
- (void)openFolderAsVault:(NSPasteboard *)board userData:(NSString *)data error:(NSString **)error;
@end

@implementation OkilumFolderService
- (void)openFolderAsVault:(NSPasteboard *)board userData:(NSString *)data error:(NSString **)error {
    // Finder's legacy filename list and modern file URLs are both supported.
    NSArray<NSURL *> *urls = [board readObjectsForClasses:@[NSURL.class] options:@{}];
    // A legacy filename representation may omit remote items from a mixed
    // modern selection; count the complete URL selection before consulting it.
    if (urls.count > 1) {
        if (error) *error = @"Select exactly one local folder.";
        return;
    }
    id paths = [board propertyListForType:@"NSFilenamesPboardType"];
    if (paths != nil) {
        if ([paths isKindOfClass:NSArray.class]) {
            NSMutableArray<NSURL *> *legacy = [NSMutableArray array];
            for (id path in paths) {
                if (![path isKindOfClass:NSString.class] || ![path isAbsolutePath]) {
                    if (error) *error = @"Select exactly one local folder.";
                    return;
                }
                [legacy addObject:[NSURL fileURLWithPath:path]];
            }
            urls = legacy;
        }
    } else {
        // Do not filter non-file URLs before counting: mixed selection is ambiguous.
        urls = [board readObjectsForClasses:@[NSURL.class] options:@{}];
    }
    NSNumber *directory = nil;
    NSURL *url = urls.count == 1 ? urls.firstObject : nil;
    if (!url.isFileURL || ![url getResourceValue:&directory forKey:NSURLIsDirectoryKey error:NULL]
        || !directory.boolValue) {
        if (error) *error = @"Select exactly one accessible local folder.";
        return;
    }
    if (!folderOpen) {
        if (error) *error = @"Okilum is not ready to open the folder.";
        return;
    }
    // No subprocess or shell: the running app queues this URL before app.run.
    folderOpen(url.absoluteString.UTF8String);
    [NSApp activateIgnoringOtherApps:YES];
}
@end

void okilum_folder_service_install(OkilumFolderOpen callback) {
    folderOpen = callback;
    static OkilumFolderService *provider;
    if (!provider) provider = [OkilumFolderService new];
    [NSApp setServicesProvider:provider];
}

#import <AppKit/AppKit.h>

typedef void (*OkilumFolderOpen)(const char *url);
static OkilumFolderOpen folderOpen;
static OkilumFolderOpen singleFileOpen;

static NSURL *singleLocalUrl(NSPasteboard *board, NSString *selectionError, NSString **error) {
    // Finder's legacy filename list and modern file URLs are both supported.
    NSArray<NSURL *> *urls = [board readObjectsForClasses:@[NSURL.class] options:@{}];
    // A legacy filename representation may omit remote items from a mixed
    // modern selection; count the complete URL selection before consulting it.
    if (urls.count > 1) {
        if (error) *error = selectionError;
        return nil;
    }
    id paths = [board propertyListForType:@"NSFilenamesPboardType"];
    if (paths != nil) {
        if ([paths isKindOfClass:NSArray.class]) {
            NSMutableArray<NSURL *> *legacy = [NSMutableArray array];
            for (id path in paths) {
                if (![path isKindOfClass:NSString.class] || ![path isAbsolutePath]) {
                    if (error) *error = selectionError;
                    return nil;
                }
                [legacy addObject:[NSURL fileURLWithPath:path]];
            }
            urls = legacy;
        }
    } else {
        // Do not filter non-file URLs before counting: mixed selection is ambiguous.
        urls = [board readObjectsForClasses:@[NSURL.class] options:@{}];
    }
    return urls.count == 1 ? urls.firstObject : nil;
}

@interface OkilumFolderService : NSObject
- (void)openFolderAsVault:(NSPasteboard *)board userData:(NSString *)data error:(NSString **)error;
- (void)openFileAsSingleFile:(NSPasteboard *)board userData:(NSString *)data error:(NSString **)error;
@end

@implementation OkilumFolderService
- (void)openFolderAsVault:(NSPasteboard *)board userData:(NSString *)data error:(NSString **)error {
    NSURL *url = singleLocalUrl(board, @"Select exactly one local folder.", error);
    if (!url) return;
    NSNumber *directory = nil;
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

- (void)openFileAsSingleFile:(NSPasteboard *)board userData:(NSString *)data error:(NSString **)error {
    NSURL *url = singleLocalUrl(board, @"Select exactly one local file.", error);
    if (!url) return;
    NSNumber *regularFile = nil;
    NSNumber *directory = nil;
    if (!url.isFileURL
        || ![url getResourceValue:&regularFile forKey:NSURLIsRegularFileKey error:NULL]
        || !regularFile.boolValue
        || ([url getResourceValue:&directory forKey:NSURLIsDirectoryKey error:NULL] && directory.boolValue)) {
        if (error) *error = @"Select exactly one accessible local file.";
        return;
    }
    if (!singleFileOpen) {
        if (error) *error = @"Okilum is not ready to open the file.";
        return;
    }
    singleFileOpen(url.absoluteString.UTF8String);
    [NSApp activateIgnoringOtherApps:YES];
}
@end

void okilum_folder_service_install(OkilumFolderOpen vaultCallback, OkilumFolderOpen singleFileCallback) {
    folderOpen = vaultCallback;
    singleFileOpen = singleFileCallback;
    static OkilumFolderService *provider;
    if (!provider) provider = [OkilumFolderService new];
    [NSApp setServicesProvider:provider];
}

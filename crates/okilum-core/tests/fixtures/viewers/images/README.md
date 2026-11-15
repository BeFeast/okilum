# JPG and BMP

Open each file, then compare the embeds. All four must be landscape with red at top left and green at bottom right. The JPEG samples cover baseline, progressive and EXIF orientation 6. The BMP is uncompressed 24-bit. All images are synthetic, created for Okilum (MIT).

![[photo.jpg]]

![[progressive.jpeg]]

![[oriented.JPG]]

![[bitmap.bmp]]


# HEIC/HEIF (#2984)

`still.heic` is a synthetic 64×48, 8-bit HEVC image, created for Okilum (MIT).
Its four flat regions are red (240,20,20) at top left, blue (50,70,180) at top
right/bottom left and green (20,240,20) at bottom right. It was encoded from
an RGB PNG with libheif 1.21.2 / x265, quality 95, pools=none, frame-threads=1.
The PNG source was drawn with those exact RGB values using Pillow.

`rotated.heic` and `mirrored.heic` retain the identical compressed HEVC data.
They add one essential `irot=1` (90° counterclockwise) or `imir=0` (top/bottom
mirror) property to `ipco`/`ipma`; enclosing box lengths and the `iloc` base
offset are adjusted. The decoded transform references are the whole baseline
pixel matrix rotated counterclockwise or reversed by row, with zero tolerance.
The two flat-region pixel references allow at most 2 RGB levels of codec error.
`multiple.heic` encodes the same RGB source plus its top/bottom reversal as two
top-level stills, using the same encoder options. It exercises numbered output
and first-still selection. No old HEIF corpus result or threshold is changed
by these controls.

![[still.heic]]

![[rotated.heic]]

![[mirrored.heic]]

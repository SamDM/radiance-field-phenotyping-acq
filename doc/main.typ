#show link: set text(fill: blue)
#show link: underline

Created on #datetime.today().display() by Sam De Meyer. \
The source of this document is on gitlab:\
https://gitlab.psb.ugent.be/samey-Utilities/radiance-field-phenotyping/-/blob/master/main.typ

#outline()

= How does the 3D reconstruction work?

In 2020, a new field in 3D computer vision kicked off with the presentation of
_neural radiance fields_ @mildenhallNeRFRepresentingScenes2020,
an entirely new method to create 3D scenes from a collection of 2D images.

Since then, the field developed rapidly, and currently a variant of this
technique called _Gaussian splatting_ @kerbl3DGaussianSplatting2023 is
among the best methods.

This method works in two steps:

*First* camera positions are calculated by
+ _keypoint matching_: looking for recognizable points across images
+ _initialization_: proposing initial 3D locations for these points based on their 2D locations across images
+ _bundle adjustment_: iteratively refining camera locations and properties until all 2D and 3D correspondenses match.

This gives, for each camera, the following properties:
- Their exact _locations_ and _rotations_ relative to each other (up to a scale factor), for each photograph
- The _focal length_: \
  this determines wether you have a wide (low focal length) or narrow/zoomed (high focal length) field of view
- The _principal point_: \
  I.e. where the focal point of the camera is relative to the image sensor
- _Lens distortion coefficients_ (i.e. how the lens slighlty curves straight lines in the image)

This technique, called _structure from motion_ exists since 1970,
it is implemented in the open source COLMAP software @schoenberger2016mvs@schoenberger2016sfm (and many other programs).

*Second* the 3D scene is recovered

A 3D scene is initialized by creating a bunch of floating colored 3D ellipses (3D
gaussians) and updating their properties to match 2D images.
I.e.: after each iteration, these ellipses are projected to the given of 2D images,
and their properties (size, rotation, color) are changed to resemble the images better.
Given enough iterations, and given enough diverse 2D viewpoints, the properties
all those ellipses will converge to a 3D structure that approximately matches the
real shape of the photographed objects.

Such a single colored ellipse is called a _Gaussian splat_,
and a 3D scene contains thousands to millions of splats.
The term _splatting_ likely originated from the process of generating 2D images
from the 3D gaussians, where each gaussian is figuratively splatted onto the
camera lens for each 2D input image, like flies splatted against a car
windshield when driving on a highway in summer.

The orignal method @kerbl3DGaussianSplatting2023 is implemented in Nerfstudio @tancikNerfstudioModularFramework2023
and Brush @brusseeArthurBrusseeBrush2025.

= Next steps

== Better Imaging Setup

Now the camera/lens combination is suboptimal. As can be seen in the input video (next section)
most of the image on each frame is background. The image resolution of the root nodules is low (@root-img).

Filming from closer will not help much because this will create a very
narrow depth of field, which will make most of the root blurry.
Instead, a zoom lens (high focal length) should be purchased, such that the
full sensor resolution can be used without introducing blur.

#figure(
  grid(columns: (1fr, 2fr), gutter: 1em)[#image("CP-176_4-input-image.jpg")][#image("CP-176_4-input-image-crop.jpg")],
  caption: [Left: full image, where most pixels are _wasted_ on background. Right: zoom of the left image, note the blurry resolution of the nodules.],
) <root-img>


== Anatomical annotation

A 3D model of a plant structure on its own is not worth much.
We need to know which parts of the model correspond to which anatomical
structures (stem, leaf, flower, main root, side root, nodule, etc.)

To get a functional 3D anatomy, we can follow 2 approaches:
1. _Projection method_: where we project 2D annotations to 3D model. \
  This involves:
  + Training a neural network on 2D images to recognize anatomical structures.
  + Using the neural network to "annotate" all images that serve as input into a _Gaussian splatting_ reconstruction.
  + Modify the _Gaussian splatting_ representation and loss function to _render_ and optimize segmentation maps as well.
    This is the preferred high-effort and high-quality approach.
    This is theoretically established for other radiance field methods, but
    not implemented for _Gaussian splatting_ yet. As such I (Sam De Meyer)
    will have to do this (and I know how, but it is a lot of work). \
    _or_ \
    Recolor the input footage for each segmentation map one-by-one. And make one 3D model per anatomical structure.
    This is doable without modifying the _Gaussian splatting_ algorithm.
    This is in fact very easy, but will result in poorer quality 3D models.

2. _3D Deep learning method_: where we train a 3D neural network to label
  anatomical structure on the gaussian splat model directly.
  A gaussian splat model is essentailly a fancy point cloud,
  hence existing point cloud labeling methods can be used with very little modification
  to annotate gaussian splats.

#table(
  columns: 3,
  [method], [pros], [cons],
  [_Projection_],
  [
    - easy labeling for making training data
    - ability to leverage pre-trained methods
  ],
  [- high programming effort],

  [_3D Deep learning_],
  [- low programming effort],
  [
    - difficult labeling
    - no good software exists yet to perform labeling
  ],
)

= File info

Files available:
- on Midas at: \ `sftp://midas/home/samey/Safe/Deliveries/2025-09-30 soybean-root-phenotyping/`
- on Floppy at: https://floppy.psb.ugent.be/index.php/f/19363974

I made a bunch of videos showing off the 3D model of the _best_ video we have,
which unfortunately is still not very sharp (i.e. the root nodules are blurry in the video).

- Renders:
  - There is:
    - `crop_0`: this shows the 3D model of the root + the entire room.
    - `crop_1`: this shows the 3D model of the root + the table and support beams.
    - `crop_2`: this shows the 3D model of the root only. \
      I also uploaded this video as separate images.
      Look at frame 36 for a good example of how well the root nodules are reconstructed in 3D.

  - There are also `depth` videos, which show a depth map (#text(fill: red)[red] = nearby, #text(fill: blue)[blue] = far)
  - There is also a side-by-side video: `2025-09-29-11-09-46_crop_2_rgb-depth.mp4`

- Videos:
  - The file `CP-176_4.mp4` is the 40 second input video. This is the *_only_* data used to create the 3D model.
    - All camera paremeters (locations and lens properties) are learned from this video only.
    - The 3D model is derived from this video only.
  - The file `Screencast 2025-09-30 14:23:33.mp4` is a screencast of me flying
    around in the 3D model using the Nerfstudio @tancikNerfstudioModularFramework2023 software.

I made a `.gif` version of each video,
because some presentation software does not work well with `.mp4` files (e.g.
google slides).
And because making high-quality/low-filesize gifs is not straightforward.

= Protocol (WIP, not up to date)

== Data Acquisition Setup

=== Acquisition room

- Print aruco markers on dibond plate, or order a plate
  - Put the plate on a table, below the plant
- Use a sample tags with a QR code + human readable sample name
- Use high-intensity studio lights, around 4 is recommended, put then om full power
  - The room must be evenly lit, no shadows
  - Avoid direct sunlight into the room
  - If the lights are spread out well, you won't cast a shadow either. This is required.
  - Avoid highly mirrored surfaces and transparent objects in the room
- Make sure there are clear landmarks all around the room in which you take the images


=== Camera setup

- Change f-stop (i.e. size of the aperture of the camera).
  We should try to set the f-stop as high as possible.
  - The lower the aperture (i.e. the higher the f-stop) the easier it is to get
    an object completely in the focus range (as opposed to having focus blur).
  - The lower the aperture, the darker the image is. Hence strong external light
    helps.
- Fix the ISO setting. The ISO is a number which determines how sensitive the
  imaging sensor is to light.
  We need to have the ISO at a constant level, because if we don't then light intensity changes over images, which is suboptimal.
  - High ISO = bright images, but more "shot noise"
  - Low ISO  = darker images, but less noisy.
- Fix the whitebalance setting. This setting determines how to weight
  red/blue/green pixels in the final image.
  To prevent color shifts between images, we need to manually set this to a fixed value.
- Fix the exposure time. This is the time the image sensor is "open" to
  accumulate light.
  Preferably we set this to a low value. Because longer exposure in handheld
  camera results in blurrier images.
  We best set this to at most a few milliseconds.
- Enable auto-focus. Such that the camera automatically keeps the central object
  "sharp", i.e. in focus.
  Filming sharp footage with manual focus is very difficult.
  Some cameras have different auto-focus modes. I.e. a mode that tries to focus
  the whole image evenly, and a mode to sharpen just the central region.
  If possible, we need the latter, because we want to make sharp images of the
  root, which will be in the center of the viewpoint most of the time.
- Enable video-recording at 3 FPS. That should give us enough data to work with if you film while walking around the root.
  If only e.g. 20 FPS is possible, that is fine too. 20FPS will result in many near-identical images, but we can just discard images such that we keep approximately 3 FPS.
- Finally, know how to transfer images to a PC.


=== Image capture

=== How to a video for 3D reconstruction

- Place the plant in position
- Place the ID tag on the table, below the plant
- Start filming with the camera, at a low framerate (e.g. 3 FPS)
  - Start your movie on the ID tag
  - Back off from the tag such that the aruco markers are in frame
  - Start aiming the camera at the plant from a bottom to top angle, at roughly 40 degrees upwards
  - Walk around the plant, keeping it in frame, keep filming from the same height and keep the same vertical angle
  - Once you did full circle around the plant, move to a top-down angle and do another full circle around the plant
  - Move that camera back towards filming the ID tag
  - End the video on the ID tag

After the first video, upload it to a PC and look at it with a high-res display.


#bibliography("references.bib")

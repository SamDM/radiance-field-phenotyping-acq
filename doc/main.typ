#show link: set text(fill: blue)
#show link: underline
#set page(numbering: "1")

Created on #datetime.today().display() by Sam De Meyer. \
The source of this document is on gitlab:\
https://gitlab.psb.ugent.be/samey-Utilities/radiance-field-phenotyping

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

= 2026: second attempt

== System architecture

=== Overview

There are two cameras mounted on an 1m diameter orbital rail.
Both cameras are connected and powered with a single RJ45 data cable to a small
_soft router_ type mini-PC.
There is a POE injector in between each camera and the mini-PC to provide them with electrical power.

The mini-PC has the following functions:
1. Run the basler software which controls the cameras, this is used to:
  1. clock-sync both cameras using NTP.
  2. trigger both cameras.
  3. acquire the raw images from the basler cameras.
2. Compress the acquired images to PNG.
3. Upload the acquired images to the PSB midas server through a wall socket.


#figure(
  {
    import "@preview/cetz:0.4.2"
    cetz.canvas({
      import cetz.draw: *

      // Styles
      let styles = (
        cam: (fill: rgb("#E0F2FD"), stroke: blue),
        poe: (fill: rgb("#FFF3E0"), stroke: orange),
        pc: (fill: rgb("#E8F5E9"), stroke: green.darken(20%)),
        wall: (fill: rgb("#F5F5F5"), stroke: gray),
      )
      let data-line = (stroke: (paint: blue, thickness: 2pt), mark: (end: ">", start: ">"))
      let power-line = (stroke: (paint: red, thickness: 2pt, dash: "dashed"))

      // Helper function to draw a labeled box
      let draw-box(pos, size, style, name, label, text-size: 7pt) = {
        let (w, h) = size
        rect((pos.at(0) - w/2, pos.at(1) - h/2), (pos.at(0) + w/2, pos.at(1) + h/2),
             ..style, radius: 0.15, name: name)
        content(pos, text(size: text-size)[#label])
      }

      // Draw orbital rail
      let rail-center = (1.5, 3)
      circle(rail-center, radius: 1.2, stroke: (paint: purple, thickness: 3pt), fill: none)
      circle(rail-center, radius: 0.5, fill: green.lighten(60%), stroke: green.darken(20%))
      content(rail-center, text(size: 7pt)[subject])
      content((1.5, 0.3), text(size: 9pt)[Orbital rail])

      // Node positions
      let cam-offset = 0.3
      let positions = (
        cam1: (2.7, 3 + cam-offset), cam2: (2.7, 3 - cam-offset),
        poe1: (5.5, 4), poe2: (5.5, 2), pc: (8.5, 3),
        dist: (8.5, 0), wlan: (11.5, 3), wpower: (11.5, 0),
      )

      // Draw nodes
      draw-box(positions.cam1, (1.1, 0.5), styles.cam, "cam1", [Camera 1])
      draw-box(positions.cam2, (1.1, 0.5), styles.cam, "cam2", [Camera 2])
      draw-box(positions.poe1, (0.7, 0.5), styles.poe, "poe1", [POE 1])
      draw-box(positions.poe2, (0.7, 0.5), styles.poe, "poe2", [POE 2])
      draw-box(positions.pc, (0.9, 0.5), styles.pc, "pc", [mini-PC])
      draw-box(positions.dist, (2.3, 0.5), styles.wall, "dist", [distribution plug])
      draw-box(positions.wlan, (2.3, 0.5), styles.wall, "wlan", [wall LAN socket])
      draw-box(positions.wpower, (2.3, 0.5), styles.wall, "wpower", [wall power socket])

      // Draw connections
      for conn in (("cam1.east", "poe1.west"), ("cam2.east", "poe2.west"),
                   ("poe1.east", "pc.west"), ("poe2.east", "pc.west"),
                   ("pc.east", "wlan.west")) {
        line(..conn, ..data-line)
      }
      for conn in (("poe1.south", "dist.north"), ("poe2.south", "dist.north"),
                   ("pc.south", "dist.north"), ("dist.east", "wpower.west")) {
        line(..conn, ..power-line)
      }
    })
  },
  caption: [
    System architecture diagram showing data (blue solid), power (red dashed)
    connections. Cameras are mounted as a vertical stereo pair on the orbital
    rail (purple ring).
  ],
)

Some additional notes:
- Hardware:
  - All components have a maximum data throughput of 2.5 Gigabit (2.5 Mbps).
    This is required to allow fast data acquistion.
    I.e. a slower connection would require slowing down the camera movement on the
    orbital rail to allow it to send data over in time.
  - The more expensive data cables from basler are used for connecting directly to the cameras.
    These are extra-strong cables with pin-screws to attach to the cameras.
    This is required because the cameras will move around, putting strain on the cables.
  - The cables between the POE switches and mini-PC
- Software:
  - The mini-PC is not very powerfull and may have difficulties compressing the
    images _live_ to PNG.  This is OK, the PC has additional time to compress and
    upload the images after each acquisiton, while the operator is changing the
    subject to be imaged.
    Given the image compression and extra time budget, we don't a high-bandwith wall socket.
  - The cameras will be set to a continuous 2FPS trigger. The images can be
    subsampled later on the Midas server.
  - I expect that around 200 12MP images (100 from each camera) will more be
    more than sufficient for a good 3D reconstruction. This is based on testing
    with a handheld camera (see e.g. https://superspl.at/view?id=201de194).
  - Dedicated software can run on the midas high performance compute cluster for
    next steps:
    1. Compressing the PNGs to a high-quality video storage format.
      This is expected to reduce storage with (roughly) a factor 20.
    2. Running the actual 3D reconstruction algorithm.
  - Altough this is not the primary goal,
    both cameras will function as a vertical stereo pair.
    If the default 3D reconstruction has shortcomings, we can still enhance the
    reconstruction with depth cues from stereo depth estimation.

=== List of components

#[
  #show table.cell.where(y: 0): set text(weight: "bold")

  #table(
    columns: 5,
    table.header([what], [item], [qty], [price], [where to acquire]),
    //
    [Camera],
    [Basler ace 2 a2A3536-9gcBAS],
    [2],
    [#sym.euro 399],
    [#link("https://www.baslerweb.com/en/shop/a2a3536-9gcbas/")],
    //
    [Lens],
    [Basler Lens C11-0824-12M f8.5mm],
    [2],
    [#sym.euro 259],
    [#link("https://www.baslerweb.com/en/shop/basler-lens-c11-0824-12m-f8-5mm")],
    //
    [Camera data cable],
    [Cable GigE, Cat6a, RJ45 sl hor/RJ45, S, 5 m],
    [2],
    [#sym.euro 19],
    [#link("https://www.baslerweb.com/en/shop/cable-gige-cat6a-rj45-sl-hor-rj45-s-5-m/")],
    //
    [POE injector],
    [TP-LINK TL-POE260S 2.5G PoE+ Injector],
    [2],
    [#sym.euro 22.48],
    [#link("https://www.amazon.com.be/-/en/TP-LINK-TL-POE260S-2-5G-PoE-Injector/dp/B0C4SN1YJG")],
    //
    [network switch #sym.arrow.l.r PC data cable],
    [C2G Netwerkkabel CAT6 0,3 meter],
    [2],
    [#sym.euro 19.99],
    [#link("https://www.coolblue.be/nl/product/960919/c2g-netwerkkabel-cat6-0-3-meter.html")],
    //
    [PC],
    [CWWK N100 fanless mini-PC - 4 x 2.5G RJ45 LAN],
    [1],
    [#sym.euro 328.74],
    [#link("https://www.amazon.com.be/-/en/Firewall-Appliance-Fanless-Computer-OPNsense/dp/B0DNXQ6GRV")],
    //
    [Mounting], [Orbital rail], [1], [], [built at VIB],
  )
]



= 2025-09: first attempt

== Need Better Imaging Setup

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


== Next step: Anatomical annotation

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

== File info

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

== Protocol

=== Data Acquisition Setup

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

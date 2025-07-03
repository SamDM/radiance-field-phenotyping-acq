= Setup

== Acquisition room

- Print aruco markers on dibond plate
  - Put the plate on a table, below the plant
- Use a sample tags with a QR code + human readable sample name
- Use high-intensity studio lights, around 4 is recommended, put then om full power
  - The room must be evenly lit, no shadows
  - Avoid direct sunlight into the room
  - If the lights are spread out well, you won't cast a shadow either. This is required.
  - Avoid highly mirrored surfaces and transparent objects in the room
- Make sure there are clear landmarks all around the room in which you take the images


== Camera setup

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


= Image capture

== How to a video for 3D reconstruction

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

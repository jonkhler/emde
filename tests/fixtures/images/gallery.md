# Image gallery

A gradient, 64×32 pixels:

![Gradient](gradient.png "A red-to-blue gradient")

A disc with a transparent background:

![Disc](disc.png)

> Quoted, a JPEG photo:
>
> ![Photo](photo.jpg)

- A listed GIF (its first frame):

  ![Animation](anim.gif)

A WebP image, and a tiny icon from a data URI:

![WebP](tiny.webp)

![Icon](data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAgAAAAICAYAAADED76LAAAAVklEQVR4Ae3AA6AkWZbG8f937o3IzKdyS2Oubdu2bdu2bdu2bWmMnpZKr54yMyLu+Xa3anqmhztr1a8OP/9o84IRvHAELxzBC0fwwhG8cAQvHMELxz8C8VAC6DUdTLoAAAAASUVORK5CYII=)

<p align="center"><img src="gradient.png" width="32" alt="Half-size gradient"></p>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="disc.png">
  <img src="gradient.png" alt="Picture that follows the background">
</picture>

Images that cannot be shown keep their alt text:

![Missing image](missing.png)

![Truncated PNG](truncated.png)

![Empty file](empty.png)

![Not an image](not-an-image.png)

![An SVG logo](logo.svg)

![Remote image](https://example.com/remote.png)

Inline images such as ![chip](icon.png) stay chips.

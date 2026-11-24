// Photo mode's picture page: the eight picture sliders and the warmth, each on its photo key.
class ModernMenuPhotoPicture extends ModernMenuPhoto;

var string                  PageKeys[9];
var localized string        PageLabels[9];
var localized string        PageHelps[9];

function AddPageRows()
{
	local int i;

	for (i = 0; i < ArrayCount(PageKeys); i++)
		AddKeySlider(PageLabels[i], PageHelps[i], PageKeys[i], 10);
}

defaultproperties
{
	TitleText="Picture"
	OtherPage=Class'ModernMenuPhotoCamera'
	PageText="Camera"
	PageHelp="Field of view, tilt, focus and framing."

	PageKeys(0)="ModernPhotoExposure"
	PageKeys(1)="ModernPhotoContrast"
	PageKeys(2)="ModernPhotoSaturation"
	PageKeys(3)="ModernPhotoWarmth"
	PageKeys(4)="ModernPhotoVignette"
	PageKeys(5)="ModernPhotoBloom"
	PageKeys(6)="ModernPhotoGodRays"
	PageKeys(7)="ModernPhotoFringe"
	PageKeys(8)="ModernPhotoSharpen"

	PageLabels(0)="Exposure"
	PageLabels(1)="Contrast"
	PageLabels(2)="Saturation"
	PageLabels(3)="Warmth"
	PageLabels(4)="Vignette"
	PageLabels(5)="Bloom"
	PageLabels(6)="Sun rays"
	PageLabels(7)="Color fringe"
	PageLabels(8)="Sharpen"

	PageHelps(0)="Makes the picture brighter or darker. 5 leaves it as it is."
	PageHelps(1)="Makes the picture punchier or flatter. 5 leaves it as it is."
	PageHelps(2)="Makes colors stronger or paler. 5 leaves them as they are."
	PageHelps(3)="Lower is colder and bluer, higher warmer and more orange. 5 leaves it as it is."
	PageHelps(4)="Darkens the corners of the picture."
	PageHelps(5)="Makes bright lights glow."
	PageHelps(6)="Beams of sunlight through the air."
	PageHelps(7)="Colored edges toward the sides of the picture, like a cheap lens."
	PageHelps(8)="Makes fine detail crisper."
}

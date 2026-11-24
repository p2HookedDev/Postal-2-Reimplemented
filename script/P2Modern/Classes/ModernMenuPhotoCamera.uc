class ModernMenuPhotoCamera extends ModernMenuPhoto;

const TiltMost = 45;
const OrbitMost = 180;
const HeightLeast = -10;
const HeightMost = 30;

var UWindowHSliderControl   TiltSlider, OrbitSlider, HeightSlider, SpeedSlider;
var UWindowCheckbox         HideBox, GridBox;
var UWindowComboControl     BarsCombo;
var ShellMenuChoice         FocusChoice, ResetCameraChoice;
var localized string        CameraLabels[12];
var localized string        CameraHelps[12];
var localized string        BarNames[3];

function AddPageRows()
{
	local int i;

	AddKeySlider(CameraLabels[0], CameraHelps[0], "ModernPhotoFov", 150);
	TiltSlider = AddPhotoSlider(CameraLabels[1], CameraHelps[1], -TiltMost, TiltMost);
	OrbitSlider = AddPhotoSlider(CameraLabels[2], CameraHelps[2], -OrbitMost, OrbitMost);
	HeightSlider = AddPhotoSlider(CameraLabels[3], CameraHelps[3], HeightLeast, HeightMost);
	SpeedSlider = AddPhotoSlider(CameraLabels[4], CameraHelps[4], 1, 10);
	FocusChoice = AddChoice(CameraLabels[5], CameraHelps[5], ItemFont, ItemAlign);
	AddKeySlider(CameraLabels[6], CameraHelps[6], "ModernPhotoFocus", 36);
	AddKeySlider(CameraLabels[7], CameraHelps[7], "ModernPhotoBlur", 10);
	HideBox = AddCheckbox(CameraLabels[8], CameraHelps[8], ItemFont);
	GridBox = AddCheckbox(CameraLabels[9], CameraHelps[9], ItemFont);
	BarsCombo = AddComboBox(CameraLabels[10], CameraHelps[10], ItemFont);
	for (i = 0; i < ArrayCount(BarNames); i++)
		BarsCombo.AddItem(BarNames[i]);
	ResetCameraChoice = AddChoice(CameraLabels[11], CameraHelps[11], ItemFont, ItemAlign);
}

function LoadSliders()
{
	Super.LoadSliders();
	if (Photo == None)
		return;
	bUpdate = false;
	SpeedSlider.SetValue(Photo.SpeedLevel, true);
	HideBox.SetValue(ReadLevel("ModernPhotoHidePlayer", 0) > 0);
	GridBox.SetValue(Photo.bGrid);
	BarsCombo.SetSelectedIndex(Photo.Bars);
	FollowCamera();
	bUpdate = true;
}

function FollowSlider(UWindowHSliderControl Slider, int Value)
{
	if (int(Slider.GetValue()) != Value)
		Slider.SetValue(Value, true);
}

function FollowCamera()
{
	FollowSlider(TiltSlider, Photo.Nearest(Photo.Tilt));
	FollowSlider(OrbitSlider, Photo.OrbitDegrees());
	FollowSlider(HeightSlider, Photo.HeightLevel());
}

function BeforePaint(Canvas C, float X, float Y)
{
	Super.BeforePaint(C, X, Y);
	if (Photo != None)
		FollowCamera();
}

function Notify(UWindowDialogControl C, byte E)
{
	Super.Notify(C, E);

	if (C == None || Photo == None)
		return;
	if (E == DE_Click)
	{
		if (C == FocusChoice)
		{
			Photo.FocusOnCenter();
			LoadSliders();
		}
		else if (C == ResetCameraChoice)
		{
			Photo.ResetCamera();
			FollowCamera();
		}
		return;
	}
	if (E != DE_Change || !bUpdate)
		return;
	if (C == TiltSlider)
		Photo.Tilt = TiltSlider.GetValue();
	else if (C == OrbitSlider)
		Photo.SetOrbit(int(OrbitSlider.GetValue()));
	else if (C == HeightSlider)
		Photo.SetHeight(int(HeightSlider.GetValue()));
	else if (C == SpeedSlider)
		Photo.SpeedLevel = int(SpeedSlider.GetValue());
	else if (C == HideBox)
	{
		if (HideBox.bChecked)
			WriteKey("ModernPhotoHidePlayer", "1");
		else
			WriteKey("ModernPhotoHidePlayer", "0");
	}
	else if (C == GridBox)
		Photo.bGrid = GridBox.bChecked;
	else if (C == BarsCombo)
		Photo.Bars = Clamp(BarsCombo.GetSelectedIndex(), 0, ArrayCount(BarNames) - 1);
}

defaultproperties
{
	TitleText="Camera"
	OtherPage=Class'ModernMenuPhotoPicture'
	PageText="Picture"
	PageHelp="Exposure, colors, warmth and lens effects."

	CameraLabels(0)="Field of view"
	CameraLabels(1)="Tilt"
	CameraLabels(2)="Orbit player"
	CameraLabels(3)="Height"
	CameraLabels(4)="Camera speed"
	CameraLabels(5)="Focus on center"
	CameraLabels(6)="Focus distance"
	CameraLabels(7)="Background blur"
	CameraLabels(8)="Hide player"
	CameraLabels(9)="Grid"
	CameraLabels(10)="Black bars"
	CameraLabels(11)="Reset camera"

	CameraHelps(0)="Zoom in or out. 0 keeps the game's own view."
	CameraHelps(1)="Rolls the camera left or right. Q and E do the same."
	CameraHelps(2)="Swings the camera around the player and turns it to face him."
	CameraHelps(3)="Moves the camera straight up or down."
	CameraHelps(4)="How fast the camera flies."
	CameraHelps(5)="Puts the focus on whatever the middle of the screen points at."
	CameraHelps(6)="How far away the sharp part of the picture is. 0 turns depth of field off."
	CameraHelps(7)="How soft everything out of focus gets."
	CameraHelps(8)="Leaves the player out of the picture."
	CameraHelps(9)="Lines that split the screen in thirds to help framing. They are not in the photo."
	CameraHelps(10)="Movie style bars across the top and bottom. They are in the photo."
	CameraHelps(11)="Puts the camera back at the player's eyes, level and untilted."

	BarNames(0)="Off"
	BarNames(1)="1.85:1"
	BarNames(2)="2.35:1"
}

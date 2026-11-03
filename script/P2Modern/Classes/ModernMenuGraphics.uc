// Each box is one render device key, read and set with `get` / `set ini:Engine.Engine.RenderDevice`.
// While the master is off the effect boxes are disabled; their stored values stay.
class ModernMenuGraphics extends ShellMenuCW;

const KeyPath = "ini:Engine.Engine.RenderDevice";

var UWindowCheckbox     Boxes[8];
var string              Keys[8];
var localized string    Labels[8];
var localized string    Helps[8];

var localized string    TitleText;
var localized string    BackHelp;
var Color               DimTextColor;
var bool                bUpdate;

function CreateMenuContents()
{
	local int i;

	Super.CreateMenuContents();

	AddTitle(TitleText, TitleFont, TitleAlign);

	bUpdate = false;
	for (i = 0; i < ArrayCount(Boxes); i++)
	{
		Boxes[i] = AddCheckbox(Labels[i], Helps[i], ItemFont);
		Boxes[i].SetValue(ReadKey(Keys[i], i > 0 && Keys[i] != "ModernAO"));
	}
	BackChoice = AddChoice(BackText, BackHelp, ItemFont, ItemAlign, true);
	ShowMaster();
	bUpdate = true;
}

function bool ReadKey(string Key, bool bMissing)
{
	local string Value;

	Value = GetPlayerOwner().ConsoleCommand("get" @ KeyPath @ Key);
	if (Value == "")
		return bMissing;
	return bool(Value);
}

function WriteKey(string Key, bool bValue)
{
	GetPlayerOwner().ConsoleCommand("set" @ KeyPath @ Key @ string(bValue));
}

function ShowMaster()
{
	local int i;
	local bool bOn;

	bOn = Boxes[0].bChecked;
	for (i = 1; i < ArrayCount(Boxes); i++)
	{
		Boxes[i].bDisabled = !bOn;
		if (bOn)
			Boxes[i].SetTextColor(ShellLookAndFeel(LookAndFeel).NormalTextColor);
		else
			Boxes[i].SetTextColor(DimTextColor);
	}
}

function Notify(UWindowDialogControl C, byte E)
{
	local int i;

	Super.Notify(C, E);

	if (C == None)
		return;
	if (E == DE_Click && C == BackChoice)
	{
		GoBack();
		return;
	}
	if (E != DE_Change || !bUpdate)
		return;
	for (i = 0; i < ArrayCount(Boxes); i++)
		if (C == Boxes[i])
		{
			WriteKey(Keys[i], Boxes[i].bChecked);
			if (i == 0)
				ShowMaster();
			return;
		}
}

defaultproperties
{
	TitleText="Modern Graphics"
	BackHelp="Return to the Options menu."

	Keys(0)="ModernGraphics"
	Keys(1)="ModernShadows"
	Keys(2)="ModernContactShadows"
	Keys(3)="ModernAO"
	Keys(4)="ModernRelief"
	Keys(5)="ModernSpecular"
	Keys(6)="ModernPost"
	Keys(7)="ModernSmoothing"

	Labels(0)="Modern graphics"
	Labels(1)="Sun shadows"
	Labels(2)="Contact shadows"
	Labels(3)="Ambient occlusion"
	Labels(4)="Surface relief"
	Labels(5)="Generated specular"
	Labels(6)="Bloom and sharpening"
	Labels(7)="Smooth transitions"

	Helps(0)="Turns all the new graphics on or off. Off gives the original look."
	Helps(1)="Shadows cast by the sun across the level."
	Helps(2)="Small shadows where objects and characters meet the ground."
	Helps(3)="Darker corners, creases and gaps, as in real light."
	Helps(4)="Bumps and depth on walls and floors, made from their own textures."
	Helps(5)="Shine on surfaces, made from their own textures."
	Helps(6)="A soft glow around bright light, smoother edges and a sharper picture."
	Helps(7)="Lighting and effects fade in smoothly instead of popping."

	DimTextColor=(R=110,G=110,B=110,A=255)

	MenuWidth=600.000000
	ItemHeight=26.000000
	ItemSpacingY=2.000000
	HintLines=3
}
